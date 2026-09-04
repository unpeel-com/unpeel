//! Core logic for `unpeel-attach`, the tmux-style attach client for Unpeel
//! hosted sessions.
//!
//! Protocol (must stay byte-compatible with
//! `apps/desktop/src-tauri/src/session_host.rs`):
//!
//! - Control socket `session.sock`: one newline-terminated JSON command per
//!   connection, answered with one newline-terminated JSON response
//!   (`{"ok":bool,"error":...}`). Commands are an internally tagged enum
//!   (`{"type":"write"|"resize"|"ping"|"kill"|...}`, snake_case).
//! - `output.bin`: logically append-only raw terminal output. Old physical
//!   blocks may be sparse; `output-retention.json` names the earliest retained
//!   lifetime offset. The attach client uses the retained tail for recovery
//!   and prefers the host's live stream for visible output, so rendering does
//!   not wait for persistence flushes.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_REPLAY_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_MUTE_INPUT_MS: u64 = 150;
pub const SOCKET_IO_TIMEOUT_MS: u64 = 1_000;
pub const OUTPUT_STREAM_IO_TIMEOUT_MS: u64 = 250;
const OUTPUT_STREAM_FRAME_HEADER_BYTES: usize = 13;
pub const INPUT_STREAM_IO_TIMEOUT_MS: u64 = 250;
const INPUT_STREAM_ACK: u8 = 0;

/// Same lookback the session host uses when aligning a tail replay start.
pub const REPLAY_TAIL_ALIGNMENT_LOOKBACK_BYTES: u64 = 16 * 1024;
const OUTPUT_RETENTION_FILE: &str = "output-retention.json";

#[derive(Debug, Deserialize)]
struct OutputRetentionState {
    version: u32,
    retained_from: u64,
}

// ---------------------------------------------------------------------------
// Socket protocol (mirrors SessionHostCommand / SessionHostResponse in
// session_host.rs; subset — we never send viewport_snapshot).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachCommand {
    Write {
        data: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Ping,
    /// Snapshot attach: the Host renders its resident VT state as VT bytes
    /// at an exact journal offset (session_host.rs `Snapshot`).
    Snapshot,
}

/// A Host VT state snapshot: apply `bytes` to a freshly reset terminal of
/// `cols`×`rows`, then stream output from `journal_offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotVt {
    pub journal_offset: u64,
    pub cols: u16,
    pub rows: u16,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotHeader {
    journal_offset: u64,
    cols: u16,
    rows: u16,
    bytes_len: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotReply {
    ok: bool,
    #[serde(default)]
    snapshot: Option<SnapshotHeader>,
}

/// Snapshot attach is on unless `UNPEEL_ATTACH_SNAPSHOT=0` (the escape hatch
/// back to raw tail replay, also the control arm of verify-attach.sh).
pub fn snapshot_attach_enabled() -> bool {
    !std::env::var("UNPEEL_ATTACH_SNAPSHOT").is_ok_and(|value| value.trim() == "0")
}

/// Ask the Host for a VT state snapshot. `Ok(None)` means "use the raw tail
/// replay instead": the Host predates the command (it closes without a reply
/// line), answered without a snapshot, or refused. Transport errors are
/// `Err` for diagnostics; callers fall back on those too.
pub fn request_snapshot(socket_path: &Path) -> Result<Option<SnapshotVt>, String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    let timeout = Some(Duration::from_millis(SOCKET_IO_TIMEOUT_MS));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let body = encode_command(&AttachCommand::Snapshot);
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to write snapshot request: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read snapshot response: {e}"))?;
    if line.trim().is_empty() {
        // Pre-snapshot Host: unknown command, connection closed.
        return Ok(None);
    }
    let Ok(reply) = serde_json::from_str::<SnapshotReply>(line.trim()) else {
        return Ok(None);
    };
    let Some(header) = reply.snapshot.filter(|_| reply.ok) else {
        return Ok(None);
    };
    let len = usize::try_from(header.bytes_len).map_err(|_| "Snapshot too large".to_string())?;
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| format!("Failed to read snapshot bytes: {e}"))?;
    Ok(Some(SnapshotVt {
        journal_offset: header.journal_offset,
        cols: header.cols,
        rows: header.rows,
        bytes,
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

pub fn encode_command(command: &AttachCommand) -> String {
    serde_json::to_string(command).expect("attach command serialization cannot fail")
}

/// Send one command over a fresh connection (the host handles exactly one
/// command per connection) and wait for its JSON response line.
pub fn send_command(socket_path: &Path, command: &AttachCommand) -> Result<(), String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    let timeout = Some(Duration::from_millis(SOCKET_IO_TIMEOUT_MS));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);

    let body = encode_command(command);
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to write command: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read host response: {e}"))?;
    let reply: HostResponse =
        serde_json::from_str(line.trim()).map_err(|e| format!("Invalid host response: {e}"))?;
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| "Session host rejected command".into()));
    }
    Ok(())
}

pub struct SessionOutputChunk {
    pub data: Vec<u8>,
    pub next_offset: u64,
    pub exited: bool,
    pub exists: bool,
}

pub enum OutputStreamRead {
    Chunk(SessionOutputChunk),
    TimedOut,
    Closed,
}

pub fn encode_output_stream_command(offset: u64) -> String {
    serde_json::json!({
        "type": "stream_output",
        "offset": offset,
        // The attach client fronts a real Ghostty surface that answers
        // terminal queries; the host stops probe-answering while one is
        // connected (see session_host's OutputQueryScanner).
        "answers_queries": true,
    })
    .to_string()
}

pub fn encode_input_stream_command() -> String {
    serde_json::json!({
        "type": "stream_input",
    })
    .to_string()
}

pub fn connect_output_stream(
    socket_path: &Path,
    offset: u64,
) -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    let _ = stream.set_write_timeout(Some(Duration::from_millis(OUTPUT_STREAM_IO_TIMEOUT_MS)));

    let body = encode_output_stream_command(offset);
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to start output stream: {e}"))?;
    Ok(stream)
}

pub fn connect_input_stream(socket_path: &Path) -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    let timeout = Some(Duration::from_millis(INPUT_STREAM_IO_TIMEOUT_MS));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);

    let body = encode_input_stream_command();
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to start input stream: {e}"))?;

    let mut ack = [0u8; 1];
    stream
        .read_exact(&mut ack)
        .map_err(|e| format!("Failed to read input stream acknowledgement: {e}"))?;
    if ack[0] != INPUT_STREAM_ACK {
        return Err("Session host rejected input stream".into());
    }

    let _ = stream.set_read_timeout(None);
    Ok(stream)
}

pub fn write_input_stream_frame<W: Write>(writer: &mut W, data: &[u8]) -> Result<(), String> {
    let len = u32::try_from(data.len()).map_err(|_| "Input frame too large".to_string())?;
    writer
        .write_all(&len.to_be_bytes())
        .and_then(|_| writer.write_all(data))
        .map_err(|e| format!("Failed to write input stream frame: {e}"))
}

pub fn read_output_stream_frame<R: Read>(reader: &mut R) -> Result<OutputStreamRead, String> {
    let mut header = [0u8; OUTPUT_STREAM_FRAME_HEADER_BYTES];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(OutputStreamRead::TimedOut);
        }
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(OutputStreamRead::Closed);
        }
        Err(error) => return Err(format!("Failed to read output stream frame: {error}")),
    }

    let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    let next_offset = u64::from_be_bytes(header[4..12].try_into().unwrap());
    let flags = header[12];
    let mut data = vec![0u8; len];
    if len > 0 {
        reader
            .read_exact(&mut data)
            .map_err(|e| format!("Failed to read output stream payload: {e}"))?;
    }

    Ok(OutputStreamRead::Chunk(SessionOutputChunk {
        data,
        next_offset,
        exited: flags & 1 != 0,
        exists: flags & 2 != 0,
    }))
}

// ---------------------------------------------------------------------------
// Manifest (read-only subset of HostedSessionManifest).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub state: ManifestState,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub terminal_modes: Option<TerminalModes>,
}

/// Input-shaping terminal modes the host's viewport parser reports as
/// currently active (mirror of `terminal_viewport::TerminalModeState`). The
/// replay reset (RIS) wipes them, and the sequences that established them
/// usually precede the replayed tail — without re-asserting them, a
/// reattached full-screen mouse app renders but never receives wheel/click
/// reports again. Absent on manifests from older hosts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TerminalModes {
    #[serde(default)]
    pub alt_screen: bool,
    #[serde(default)]
    pub set: Vec<u16>,
    #[serde(default)]
    pub reset: Vec<u16>,
}

impl TerminalModes {
    /// Escape preamble re-establishing these modes on a freshly reset
    /// terminal. Alt screen first so the tail replay draws into the screen
    /// the workload considers active.
    pub fn restore_sequence(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.alt_screen {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        for mode in &self.set {
            out.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
        }
        for mode in &self.reset {
            out.extend_from_slice(format!("\x1b[?{mode}l").as_bytes());
        }
        out
    }
}

/// The mode-restore preamble to emit between a replay reset and the tail
/// replay, read fresh from the session's manifest. Empty when the host is
/// too old to publish modes or everything matches reset defaults.
pub fn mode_restore_preamble(session_dir: &Path) -> Vec<u8> {
    load_manifest(session_dir)
        .and_then(|manifest| manifest.terminal_modes)
        .map(|modes| modes.restore_sequence())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    Running,
    Exited,
}

pub fn load_manifest(session_dir: &Path) -> Option<Manifest> {
    let raw = std::fs::read(session_dir.join("manifest.json")).ok()?;
    serde_json::from_slice(&raw).ok()
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0
        || matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
}

/// A session host is considered dead when its manifest is gone, says exited,
/// or names a pid that no longer exists.
pub fn host_is_alive(session_dir: &Path) -> bool {
    match load_manifest(session_dir) {
        Some(manifest) => {
            if manifest.state != ManifestState::Running {
                return false;
            }
            if manifest.pid.is_some_and(process_exists) {
                return true;
            }
            ping_session_host(session_dir)
        }
        None => ping_session_host(session_dir),
    }
}

fn ping_session_host(session_dir: &Path) -> bool {
    send_command(&session_dir.join("session.sock"), &AttachCommand::Ping).is_ok()
}

// ---------------------------------------------------------------------------
// Replay tail alignment (port of align_replay_tail_start in session_host.rs).
// ---------------------------------------------------------------------------

fn is_utf8_continuation_byte(byte: u8) -> bool {
    (byte & 0b1100_0000) == 0b1000_0000
}

#[derive(Clone, Copy)]
enum ReplayScanState {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    Dcs,
    DcsEscape,
    SosPmApc,
    SosPmApcEscape,
}

/// Scan `window` (bytes at absolute offsets `[scan_start, desired_start)`)
/// and return the last safe boundary at or before `desired_start`: never in
/// the middle of an escape sequence or a multi-byte UTF-8 character.
pub fn align_tail_start_in_window(window: &[u8], scan_start: u64, desired_start: u64) -> u64 {
    let mut initial_index = 0usize;
    while initial_index < window.len() && is_utf8_continuation_byte(window[initial_index]) {
        initial_index += 1;
    }

    let mut last_boundary = scan_start + initial_index as u64;
    let mut state = ReplayScanState::Ground;

    for (index, byte) in window.iter().enumerate().skip(initial_index) {
        let absolute = scan_start + index as u64;
        if matches!(*byte, 0x18 | 0x1a) {
            last_boundary = absolute + 1;
            state = ReplayScanState::Ground;
            continue;
        }
        if *byte == 0x1b
            && matches!(
                state,
                ReplayScanState::Escape
                    | ReplayScanState::EscapeIntermediate
                    | ReplayScanState::Csi
            )
        {
            last_boundary = absolute;
            state = ReplayScanState::Escape;
            continue;
        }
        match state {
            ReplayScanState::Ground => match *byte {
                0x1b => {
                    last_boundary = absolute;
                    state = ReplayScanState::Escape;
                }
                b'\n' | b'\r' => {
                    last_boundary = absolute + 1;
                }
                _ => {}
            },
            ReplayScanState::Escape => match *byte {
                b'[' => state = ReplayScanState::Csi,
                b']' => state = ReplayScanState::Osc,
                b'P' => state = ReplayScanState::Dcs,
                b'X' | b'^' | b'_' => state = ReplayScanState::SosPmApc,
                0x20..=0x2f => state = ReplayScanState::EscapeIntermediate,
                _ => {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
            },
            ReplayScanState::EscapeIntermediate => {
                if (0x30..=0x7e).contains(byte) {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
            }
            ReplayScanState::Csi => {
                if (0x40..=0x7e).contains(byte) {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
            }
            ReplayScanState::Osc => match *byte {
                0x07 => {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
                0x1b => state = ReplayScanState::OscEscape,
                _ => {}
            },
            ReplayScanState::OscEscape => {
                if *byte == b'\\' {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                } else {
                    state = ReplayScanState::Osc;
                }
            }
            ReplayScanState::Dcs => {
                if *byte == 0x1b {
                    state = ReplayScanState::DcsEscape;
                }
            }
            ReplayScanState::DcsEscape => {
                if *byte == b'\\' {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                } else {
                    state = ReplayScanState::Dcs;
                }
            }
            ReplayScanState::SosPmApc => {
                if *byte == 0x1b {
                    state = ReplayScanState::SosPmApcEscape;
                }
            }
            ReplayScanState::SosPmApcEscape => {
                if *byte == b'\\' {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                } else {
                    state = ReplayScanState::SosPmApc;
                }
            }
        }
    }

    last_boundary.min(desired_start)
}

pub fn align_replay_tail_start(file: &mut File, desired_start: u64) -> Result<u64, String> {
    align_replay_tail_start_from(file, desired_start, 0)
}

fn align_replay_tail_start_from(
    file: &mut File,
    desired_start: u64,
    retained_from: u64,
) -> Result<u64, String> {
    if desired_start <= retained_from {
        return Ok(retained_from);
    }

    let scan_start = desired_start
        .saturating_sub(REPLAY_TAIL_ALIGNMENT_LOOKBACK_BYTES)
        .max(retained_from);
    let scan_len = desired_start.saturating_sub(scan_start) as usize;
    if scan_len == 0 {
        return Ok(desired_start);
    }

    file.seek(SeekFrom::Start(scan_start))
        .map_err(|e| format!("Failed to seek output log for replay alignment: {e}"))?;
    let mut window = vec![0u8; scan_len];
    file.read_exact(&mut window)
        .map_err(|e| format!("Failed to read output log for replay alignment: {e}"))?;

    Ok(align_tail_start_in_window(
        &window,
        scan_start,
        desired_start,
    ))
}

fn retained_from_for_output_at_len(output_path: &Path, file_len: u64) -> u64 {
    let Some(parent) = output_path.parent() else {
        return 0;
    };
    std::fs::read(parent.join(OUTPUT_RETENTION_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<OutputRetentionState>(&bytes).ok())
        .filter(|state| state.version == 1)
        .map(|state| state.retained_from.min(file_len))
        .unwrap_or(0)
}

/// Earliest logical byte still readable from this output journal. Legacy or
/// malformed metadata means a fully retained file; values beyond the current
/// logical end are clamped while a replacement Host initializes its epoch.
pub fn output_retained_from(output_path: &Path) -> u64 {
    let file_len = std::fs::metadata(output_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    retained_from_for_output_at_len(output_path, file_len)
}

/// Read the boundary-aligned tail of `output.bin`. Returns the tail bytes and
/// the absolute offset just past them (the offset live following starts at).
pub fn read_replay_tail(output_path: &Path, replay_bytes: u64) -> Result<(Vec<u8>, u64), String> {
    read_replay_tail_with_retries(output_path, replay_bytes, 3)
}

fn read_replay_tail_with_retries(
    output_path: &Path,
    replay_bytes: u64,
    retries: u8,
) -> Result<(Vec<u8>, u64), String> {
    let mut file = match File::open(output_path) {
        Ok(file) => file,
        // Host may not have produced output yet; start following from zero.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), 0));
        }
        Err(error) => return Err(format!("Failed to open output log: {error}")),
    };
    let file_len = file
        .metadata()
        .map_err(|e| format!("Failed to stat output log: {e}"))?
        .len();

    let retained_from = retained_from_for_output_at_len(output_path, file_len);
    let desired_start = file_len.saturating_sub(replay_bytes).max(retained_from);
    let start = align_replay_tail_start_from(&mut file, desired_start, retained_from)?;

    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Failed to seek output log: {e}"))?;
    // Read exactly up to the length we measured; the file may keep growing
    // underneath us and live following picks up from `file_len`.
    let mut data = vec![0u8; (file_len - start) as usize];
    file.read_exact(&mut data)
        .map_err(|e| format!("Failed to read output log: {e}"))?;
    let newest_file_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(file_len);
    let newest_retained_from = retained_from_for_output_at_len(output_path, newest_file_len);
    if start < newest_retained_from {
        if retries > 0 {
            return read_replay_tail_with_retries(output_path, replay_bytes, retries - 1);
        }
        // Never pass a sample that may have raced hole punching. An empty
        // replay at the newest end is safe; live following supplies whatever
        // arrives after this unusually hot retention window.
        return Ok((Vec::new(), newest_file_len));
    }
    Ok((data, file_len))
}

// ---------------------------------------------------------------------------
// stdin → write-command framing.
// ---------------------------------------------------------------------------

/// The host's write command carries a JSON string, so input must be valid
/// UTF-8 on the wire (the host does `data.as_bytes()` on the decoded string).
/// Splits `pending` into the longest valid-UTF-8 prefix plus held-back bytes:
/// an incomplete trailing multi-byte sequence is kept for the next read so it
/// is relayed intact once complete. Truly invalid bytes cannot be represented
/// in this protocol at all and are dropped — terminal keyboard input is
/// UTF-8 + ASCII escape sequences in practice, so this does not occur for
/// real keystrokes.
pub fn split_valid_utf8(pending: &[u8]) -> (String, Vec<u8>) {
    match std::str::from_utf8(pending) {
        Ok(text) => (text.to_string(), Vec::new()),
        Err(error) => {
            let valid_len = error.valid_up_to();
            let valid = std::str::from_utf8(&pending[..valid_len])
                .unwrap()
                .to_string();
            match error.error_len() {
                // Incomplete sequence at the tail: hold it back.
                None => (valid, pending[valid_len..].to_vec()),
                // Invalid byte mid-stream: skip it and keep the remainder
                // pending for re-examination.
                Some(bad_len) => {
                    let mut rest = (valid, pending[valid_len + bad_len..].to_vec());
                    // Recurse so a single bad byte does not stall the stream.
                    let (more, held) = split_valid_utf8(&rest.1);
                    rest.0.push_str(&more);
                    (rest.0, held)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Focus-event filtering (always on unless --forward-focus-events).
// ---------------------------------------------------------------------------

/// Drops terminal focus-report events from the stdin → socket path.
///
/// Replayed output can re-enable focus reporting (`CSI ? 1004 h`) in the
/// *new* terminal rendering us, which then emits `ESC [ I` (focus in) /
/// `ESC [ O` (focus out) on every focus change — at any time, long after the
/// post-replay mute window has closed. Forwarding them duplicates reports the
/// workload's own terminal state never asked us for and, worse, they render
/// as literal `^[[I^[[O` text inside TUIs that do not consume them.
///
/// This filter drops exactly the parameterless 3-byte sequences
/// `ESC [ I` and `ESC [ O`, even when split across read chunks, and passes
/// everything else through unchanged: arrow keys (`ESC [ A`..`D`), function
/// keys, and parameterized CSI sequences that merely *end* in `I`/`O`
/// (e.g. `ESC [ 2 I`, cursor-forward-tab) are forwarded byte-for-byte.
///
/// Because a chunk may end mid-prefix (`ESC` or `ESC [`), up to two bytes are
/// held back until the next chunk decides their fate. Call [`Self::flush`] on
/// EOF so a trailing held-back prefix is not silently dropped.
pub struct FocusEventFilter {
    state: FocusState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusState {
    Ground,
    /// Seen `ESC`, holding it back.
    Esc,
    /// Seen `ESC [`, holding both back.
    EscBracket,
}

impl Default for FocusEventFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusEventFilter {
    pub fn new() -> Self {
        Self {
            state: FocusState::Ground,
        }
    }

    /// Filter one chunk, returning the bytes to forward. Held-back prefix
    /// bytes from a previous chunk are emitted here as soon as a byte proves
    /// they are not part of a focus event.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() + 2);
        for &byte in input {
            match self.state {
                FocusState::Ground => {
                    if byte == 0x1b {
                        self.state = FocusState::Esc;
                    } else {
                        out.push(byte);
                    }
                }
                FocusState::Esc => match byte {
                    b'[' => self.state = FocusState::EscBracket,
                    // ESC ESC: the first ESC is plain input; the new one may
                    // still open a focus event.
                    0x1b => out.push(0x1b),
                    _ => {
                        out.push(0x1b);
                        out.push(byte);
                        self.state = FocusState::Ground;
                    }
                },
                FocusState::EscBracket => match byte {
                    // The exact focus events: drop all three bytes.
                    b'I' | b'O' => self.state = FocusState::Ground,
                    // Anything else (parameters, intermediates, other finals,
                    // e.g. arrows or `ESC [ 2 I`): not a focus event — release
                    // the held prefix and the byte unchanged.
                    0x1b => {
                        out.extend_from_slice(&[0x1b, b'[']);
                        self.state = FocusState::Esc;
                    }
                    _ => {
                        out.extend_from_slice(&[0x1b, b'[']);
                        out.push(byte);
                        self.state = FocusState::Ground;
                    }
                },
            }
        }
        out
    }

    /// Release any held-back prefix (a trailing lone `ESC` or `ESC [` that no
    /// further byte will ever resolve). Call on stdin EOF.
    pub fn flush(&mut self) -> Vec<u8> {
        let out = match self.state {
            FocusState::Ground => Vec::new(),
            FocusState::Esc => vec![0x1b],
            FocusState::EscBracket => vec![0x1b, b'['],
        };
        self.state = FocusState::Ground;
        out
    }
}

// ---------------------------------------------------------------------------
// Replay-window input muting.
// ---------------------------------------------------------------------------

/// Replayed output can contain terminal queries (DA, DECRQM, DSR/CPR, ...)
/// that the *new* terminal rendering us will answer on our stdin. Those
/// answers are not user keystrokes and must not reach the workload — it
/// already received the real answers from the original terminal when the
/// bytes were first produced. During a short mute window after replay we
/// strip ESC-prefixed sequences from stdin while letting plain keystrokes
/// through.
///
/// Known limitation (acceptable for the spike): real user escape-key input
/// (arrows, alt-chords) typed inside the mute window is dropped too, and a
/// query emitted by the workload *after* the window closes will be answered
/// twice (once by the old terminal via replay, once live). Both windows are
/// ~150 ms; in practice this only matters for adversarial typing.
pub struct MuteFilter {
    state: MuteState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MuteState {
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
    Dcs,
    DcsEscape,
    Ss3,
}

impl Default for MuteFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl MuteFilter {
    pub fn new() -> Self {
        Self {
            state: MuteState::Ground,
        }
    }

    /// True when not in the middle of a partially consumed escape sequence.
    /// Callers keep filtering past the mute deadline until this returns true
    /// so a sequence spanning the window edge is not half-forwarded.
    pub fn is_ground(&self) -> bool {
        self.state == MuteState::Ground
    }

    /// Filter one chunk, returning only the bytes that should be forwarded.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &byte in input {
            match self.state {
                MuteState::Ground => {
                    if byte == 0x1b {
                        self.state = MuteState::Escape;
                    } else {
                        out.push(byte);
                    }
                }
                MuteState::Escape => {
                    self.state = match byte {
                        b'[' => MuteState::Csi,
                        b']' => MuteState::Osc,
                        b'P' => MuteState::Dcs,
                        b'O' => MuteState::Ss3,
                        // Two-byte ESC sequence: drop both bytes.
                        _ => MuteState::Ground,
                    };
                }
                MuteState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = MuteState::Ground;
                    }
                }
                MuteState::Osc => match byte {
                    0x07 => self.state = MuteState::Ground,
                    0x1b => self.state = MuteState::OscEscape,
                    _ => {}
                },
                MuteState::OscEscape => {
                    self.state = if byte == b'\\' {
                        MuteState::Ground
                    } else {
                        MuteState::Osc
                    };
                }
                MuteState::Dcs => {
                    if byte == 0x1b {
                        self.state = MuteState::DcsEscape;
                    }
                }
                MuteState::DcsEscape => {
                    self.state = if byte == b'\\' {
                        MuteState::Ground
                    } else {
                        MuteState::Dcs
                    };
                }
                MuteState::Ss3 => {
                    // SS3 final byte: drop it and return to ground.
                    self.state = MuteState::Ground;
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Raw-mode terminal guard.
// ---------------------------------------------------------------------------

/// Compute the raw-mode termios used while attached, from the current
/// settings. Equivalent to cfmakeraw(3): no canonical line buffering, no
/// local echo (the hosted PTY does the echoing), no signal generation or
/// flow control (Ctrl-C/Ctrl-S/... are forwarded to the workload as bytes),
/// no output post-processing (`output.bin` bytes are already terminal-ready),
/// and reads return as soon as one byte is available.
pub fn make_raw_termios(mut termios: libc::termios) -> libc::termios {
    unsafe { libc::cfmakeraw(&mut termios) };
    termios.c_cc[libc::VMIN] = 1;
    termios.c_cc[libc::VTIME] = 0;
    termios
}

/// Switches a tty fd into raw mode for the lifetime of the attach and
/// restores the original settings on drop.
///
/// This is load-bearing, not cosmetic: the PTY a terminal surface spawns us
/// on starts in *canonical* mode with ECHO/ECHOCTL. Without raw mode, escape
/// keys (arrows, etc.) are echoed locally by the kernel as literal `^[[A`
/// text and all input is line-buffered until Enter instead of reaching the
/// hosted workload per keystroke.
pub struct RawModeGuard {
    fd: libc::c_int,
    original: libc::termios,
}

impl RawModeGuard {
    /// Enable raw mode on `fd`. Returns `None` when `fd` is not a tty (e.g.
    /// piped stdin in tests) or the switch fails; the attach then runs
    /// unmodified, which is correct for non-tty transports.
    pub fn enable(fd: libc::c_int) -> Option<Self> {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return None;
        }
        let raw = make_raw_termios(original);
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
    }
}

// ---------------------------------------------------------------------------
// Terminal size.
// ---------------------------------------------------------------------------

/// Query the size of our own terminal via TIOCGWINSZ on stdout. Returns None
/// when stdout is not a tty (e.g. piped in tests).
pub fn terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_command_encoding_matches_host_protocol() {
        assert_eq!(
            encode_command(&AttachCommand::Write {
                data: "hello".into()
            }),
            r#"{"type":"write","data":"hello"}"#
        );
    }

    #[test]
    fn resize_command_encoding_matches_host_protocol() {
        assert_eq!(
            encode_command(&AttachCommand::Resize {
                cols: 120,
                rows: 40
            }),
            r#"{"type":"resize","cols":120,"rows":40}"#
        );
    }

    #[test]
    fn ping_command_encoding_matches_host_protocol() {
        assert_eq!(encode_command(&AttachCommand::Ping), r#"{"type":"ping"}"#);
    }

    #[test]
    fn output_stream_command_encoding_matches_host_protocol() {
        assert_eq!(
            encode_output_stream_command(42),
            r#"{"answers_queries":true,"offset":42,"type":"stream_output"}"#
        );
    }

    #[test]
    fn input_stream_command_encoding_matches_host_protocol() {
        assert_eq!(encode_input_stream_command(), r#"{"type":"stream_input"}"#);
    }

    #[test]
    fn input_stream_frame_encoding_matches_host_protocol() {
        let mut bytes = Vec::new();
        write_input_stream_frame(&mut bytes, b"hello").unwrap();
        assert_eq!(&bytes[..4], &(5u32).to_be_bytes());
        assert_eq!(&bytes[4..], b"hello");
    }

    #[test]
    fn output_stream_frame_decodes_host_protocol() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(5u32).to_be_bytes());
        frame.extend_from_slice(&(123u64).to_be_bytes());
        frame.push(0b10);
        frame.extend_from_slice(b"hello");

        match read_output_stream_frame(&mut std::io::Cursor::new(frame)).unwrap() {
            OutputStreamRead::Chunk(chunk) => {
                assert_eq!(chunk.data, b"hello");
                assert_eq!(chunk.next_offset, 123);
                assert!(!chunk.exited);
                assert!(chunk.exists);
            }
            _ => panic!("expected output stream chunk"),
        }
    }

    #[test]
    fn manifest_parses_host_written_json() {
        let raw = r#"{
            "session": {"id": "s", "project_id": "p", "label": "l", "custom_title": false,
                        "command": "claude", "created_at": 1, "tag_id": null,
                        "worktree_path": null, "worktree_branch": null},
            "cwd": "/tmp",
            "state": "running",
            "pid": 42,
            "exit_code": null,
            "heartbeat_at": 1,
            "updated_at": 1
        }"#;
        let manifest: Manifest = serde_json::from_str(raw).unwrap();
        assert_eq!(manifest.state, ManifestState::Running);
        assert_eq!(manifest.pid, Some(42));
    }

    #[test]
    fn alignment_moves_start_out_of_escape_sequence() {
        let data = b"before\r\nprefix \x1b[31mRED\x1b[0m tail\r\n";
        let escape_index = data.iter().position(|b| *b == 0x1b).unwrap() as u64;
        // Desired start lands inside "\x1b[31m".
        let desired = escape_index + 2;
        let aligned = align_tail_start_in_window(&data[..desired as usize], 0, desired);
        assert_eq!(aligned, escape_index);
    }

    #[test]
    fn alignment_moves_start_out_of_utf8_sequence() {
        let data = "before\r\n🙂 emoji tail\r\n".as_bytes();
        let emoji_index = data.windows(4).position(|w| w == "🙂".as_bytes()).unwrap() as u64;
        // Desired start lands inside the 4-byte emoji.
        let desired = emoji_index + 1;
        let aligned = align_tail_start_in_window(&data[..desired as usize], 0, desired);
        // Last boundary before the emoji is just after "before\r\n".
        assert_eq!(aligned, "before\r\n".len() as u64);
    }

    #[test]
    fn alignment_prefers_newline_boundary() {
        let data = b"line one\r\nline two";
        let desired = (data.len() - 3) as u64;
        let aligned = align_tail_start_in_window(&data[..desired as usize], 0, desired);
        assert_eq!(aligned, b"line one\r\n".len() as u64);
    }

    #[test]
    fn alignment_does_not_split_escape_intermediate_or_restarted_csi() {
        let intermediate = b"before\r\n\x1b(Bafter";
        let escape = intermediate.iter().position(|byte| *byte == 0x1b).unwrap() as u64;
        assert_eq!(
            align_tail_start_in_window(&intermediate[..escape as usize + 2], 0, escape + 2),
            escape
        );

        let restarted = b"before\r\n\x1b[1;\x1b[31mred";
        let escapes = restarted
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == 0x1b).then_some(index as u64))
            .collect::<Vec<_>>();
        assert_eq!(
            align_tail_start_in_window(&restarted[..escapes[1] as usize + 3], 0, escapes[1] + 3),
            escapes[1]
        );
    }

    #[test]
    fn replay_tail_honors_retention_floor_and_skips_sparse_prefix() {
        let temp = std::env::temp_dir().join(format!(
            "unpeel-attach-retention-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let output = temp.join("output.bin");
        let floor = 8192u64;
        let tail = b"retained line\r\n";
        let mut sparse = vec![0u8; floor as usize];
        sparse.extend_from_slice(tail);
        std::fs::write(&output, sparse).unwrap();
        std::fs::write(
            temp.join(OUTPUT_RETENTION_FILE),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "retained_from": floor,
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(output_retained_from(&output), floor);
        let (replay, next_offset) = read_replay_tail(&output, u64::MAX).unwrap();
        assert_eq!(replay, tail);
        assert_eq!(next_offset, floor + tail.len() as u64);
        assert!(!replay.contains(&0));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn split_valid_utf8_holds_back_incomplete_sequence() {
        let mut bytes = b"abc".to_vec();
        bytes.extend_from_slice(&"🙂".as_bytes()[..2]);
        let (valid, held) = split_valid_utf8(&bytes);
        assert_eq!(valid, "abc");
        assert_eq!(held, &"🙂".as_bytes()[..2]);
    }

    #[test]
    fn split_valid_utf8_drops_invalid_byte_without_stalling() {
        let bytes = b"ab\xffcd".to_vec();
        let (valid, held) = split_valid_utf8(&bytes);
        assert_eq!(valid, "abcd");
        assert!(held.is_empty());
    }

    #[test]
    fn mute_filter_drops_query_responses_passes_keystrokes() {
        let mut filter = MuteFilter::new();
        // DECRPM response + CPR response interleaved with typed characters.
        let input = b"a\x1b[?2026;1$yb\x1b[24;80Rc";
        assert_eq!(filter.filter(input), b"abc");
        assert!(filter.is_ground());
    }

    #[test]
    fn mute_filter_handles_sequence_split_across_chunks() {
        let mut filter = MuteFilter::new();
        assert_eq!(filter.filter(b"x\x1b[24;"), b"x");
        assert!(!filter.is_ground());
        assert_eq!(filter.filter(b"80Ry"), b"y");
        assert!(filter.is_ground());
    }

    #[test]
    fn mute_filter_drops_da_response() {
        let mut filter = MuteFilter::new();
        // Primary DA response.
        assert_eq!(filter.filter(b"\x1b[?62;22ctyped"), b"typed");
    }

    #[test]
    fn focus_filter_drops_exact_focus_in_and_out() {
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"\x1b[I"), b"");
        assert_eq!(filter.filter(b"\x1b[O"), b"");
        assert!(filter.flush().is_empty());
    }

    #[test]
    fn focus_filter_drops_events_split_across_chunks() {
        // Split after ESC.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"a\x1b"), b"a");
        assert_eq!(filter.filter(b"[Ib"), b"b");

        // Split after ESC [.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"c\x1b["), b"c");
        assert_eq!(filter.filter(b"Od"), b"d");

        // One byte per read.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"\x1b"), b"");
        assert_eq!(filter.filter(b"["), b"");
        assert_eq!(filter.filter(b"I"), b"");
        assert!(filter.flush().is_empty());
    }

    #[test]
    fn focus_filter_passes_arrow_keys_through() {
        let mut filter = FocusEventFilter::new();
        assert_eq!(
            filter.filter(b"\x1b[A\x1b[B\x1b[C\x1b[D"),
            b"\x1b[A\x1b[B\x1b[C\x1b[D"
        );

        // Arrow split across chunks must come out intact.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"\x1b["), b"");
        assert_eq!(filter.filter(b"A"), b"\x1b[A");
    }

    #[test]
    fn focus_filter_passes_parameterized_csi_ending_in_focus_finals() {
        let mut filter = FocusEventFilter::new();
        // Cursor-forward-tab: ends in I but has a parameter — not a focus event.
        assert_eq!(filter.filter(b"\x1b[2I"), b"\x1b[2I");
        assert_eq!(filter.filter(b"\x1b[5O"), b"\x1b[5O");
    }

    #[test]
    fn focus_filter_handles_mixed_buffers() {
        let mut filter = FocusEventFilter::new();
        let input = b"ab\x1b[Icd\x1b[A\x1b[Oef\x1b[2Ig";
        assert_eq!(filter.filter(input), b"abcd\x1b[Aef\x1b[2Ig");
    }

    #[test]
    fn focus_filter_releases_lone_escape_via_flush() {
        // A pasted/typed bare ESC at end of stream is held back, then flushed.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"x\x1b"), b"x");
        assert_eq!(filter.flush(), b"\x1b");

        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"\x1b["), b"");
        assert_eq!(filter.flush(), b"\x1b[");
    }

    #[test]
    fn raw_termios_disables_canonical_mode_echo_and_signals() {
        // Pins the arrow-key bug: a fresh terminal-surface PTY starts in
        // canonical mode with ECHO, which renders Up/Down as literal ^[[A
        // (kernel caret echo) and line-buffers input until Enter. The raw
        // termios attach installs must clear all of that and deliver input
        // byte-by-byte.
        let mut canonical: libc::termios = unsafe { std::mem::zeroed() };
        canonical.c_lflag = libc::ICANON | libc::ECHO | libc::ECHOCTL | libc::ISIG | libc::IEXTEN;
        canonical.c_iflag = libc::ICRNL | libc::IXON;
        canonical.c_oflag = libc::OPOST;

        let raw = make_raw_termios(canonical);
        // ECHOCTL may survive cfmakeraw (it does on macOS) but is inert once
        // ECHO itself is off, so it is deliberately not asserted here.
        assert_eq!(
            raw.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN),
            0,
            "raw mode must disable canonical buffering, echo, and local signals"
        );
        assert_eq!(
            raw.c_iflag & (libc::ICRNL | libc::IXON),
            0,
            "raw mode must disable CR translation and flow control"
        );
        assert_eq!(
            raw.c_oflag & libc::OPOST,
            0,
            "output must be byte-transparent"
        );
        assert_eq!(raw.c_cc[libc::VMIN], 1, "reads must return per byte");
        assert_eq!(raw.c_cc[libc::VTIME], 0);
    }

    #[test]
    fn focus_filter_handles_escape_before_focus_event() {
        // ESC ESC [ I: the first ESC is real input, the trailing focus event
        // is dropped.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"\x1b\x1b[I"), b"\x1b");

        // ESC [ ESC [ O: held prefix released when the inner ESC arrives,
        // the focus event itself dropped.
        let mut filter = FocusEventFilter::new();
        assert_eq!(filter.filter(b"\x1b[\x1b[Oz"), b"\x1b[z");
    }
}
