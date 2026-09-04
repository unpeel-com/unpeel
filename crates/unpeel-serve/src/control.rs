//! Session control-socket client for the TUI's preview and interactive input.
//!
//! Viewport fetches read the session's TRUE grid (`cols`/`rows` 0 = "current
//! size" per the host contract) without resizing it. Interactive input uses
//! the host's persistent raw stream, matching the native attach client.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use unpeel_core::terminal_viewport::TerminalViewportSnapshot;

const SOCKET_IO_TIMEOUT: Duration = Duration::from_millis(1_000);
const INPUT_STREAM_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(250);
const INPUT_STREAM_RETRY_DELAY: Duration = Duration::from_secs(5);
const INPUT_STREAM_ACK: u8 = 0;
const INPUT_BATCH_MAX_BYTES: usize = 64 * 1024;
const INPUT_STREAM_FRAME_MAX_BYTES: usize = 256 * 1024;

struct InputRequest {
    session_dir: PathBuf,
    data: Vec<u8>,
}

/// Non-blocking input path for the focused terminal.
///
/// The app's attach client keeps a `stream_input` socket open and writes
/// length-prefixed raw frames. The TUI used to open a control connection and
/// wait for a JSON response for every key and mouse-wheel tick, directly on
/// its render thread. A precision-scroll burst could therefore keep the
/// event queue non-empty faster than the UI could draw it. This sender gives
/// the TUI the same persistent path while retaining one-command `write`
/// fallback for hosts that predate `stream_input`.
pub struct InteractiveInput {
    tx: mpsc::Sender<InputRequest>,
}

impl Default for InteractiveInput {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractiveInput {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || run_input_writer(rx));
        Self { tx }
    }

    /// Queue raw terminal bytes without waiting on the session host. Nearby
    /// events are coalesced by the worker, matching the attach client's raw
    /// stdin chunking and keeping trackpad bursts cheap.
    pub fn send(&self, session_dir: &Path, data: impl AsRef<[u8]>) -> Result<(), String> {
        self.tx
            .send(InputRequest {
                session_dir: session_dir.to_path_buf(),
                data: data.as_ref().to_vec(),
            })
            .map_err(|_| "terminal input worker stopped".to_string())
    }
}

fn connect_input_stream(session_dir: &Path, initial_data: &[u8]) -> Result<UnixStream, String> {
    let socket_path = session_dir.join("session.sock");
    let mut stream = UnixStream::connect(&socket_path)
        .map_err(|e| format!("connect {}: {e}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(INPUT_STREAM_HANDSHAKE_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    // Send the triggering key batch before waiting for the acknowledgement.
    // Hosts predating the accepted-socket blocking fix could ACK and then
    // observe WouldBlock before this first frame arrived, close the stream,
    // and still let our later local write appear successful. Preframing is
    // valid for every host version and keeps already-running old sessions
    // lossless without requiring a conversation restart. Put the command and
    // first frame in one write so a nonblocking legacy host cannot parse the
    // command between two client writes and race the frame.
    if initial_data.len() > INPUT_STREAM_FRAME_MAX_BYTES {
        return Err("terminal input frame too large".to_string());
    }
    let initial_len = u32::try_from(initial_data.len())
        .map_err(|_| "terminal input frame too large".to_string())?;
    let mut preframed = Vec::with_capacity(30 + initial_data.len());
    preframed.extend_from_slice(b"{\"type\":\"stream_input\"}\n");
    preframed.extend_from_slice(&initial_len.to_be_bytes());
    preframed.extend_from_slice(initial_data);
    stream
        .write_all(&preframed)
        .map_err(|e| format!("start input stream: {e}"))?;
    let mut ack = [0u8; 1];
    stream
        .read_exact(&mut ack)
        .map_err(|e| format!("read input stream acknowledgement: {e}"))?;
    if ack[0] != INPUT_STREAM_ACK {
        return Err("session host rejected input stream".into());
    }
    let _ = stream.set_read_timeout(None);
    Ok(stream)
}

fn write_input_frame(stream: &mut UnixStream, data: &[u8]) -> Result<(), String> {
    for chunk in data.chunks(INPUT_STREAM_FRAME_MAX_BYTES) {
        let len = u32::try_from(chunk.len()).map_err(|_| "terminal input frame too large")?;
        stream
            .write_all(&len.to_be_bytes())
            .and_then(|_| stream.write_all(chunk))
            .map_err(|e| format!("write input stream: {e}"))?;
    }
    Ok(())
}

fn run_input_writer(rx: mpsc::Receiver<InputRequest>) {
    let mut pending: Option<InputRequest> = None;
    let mut active_dir: Option<PathBuf> = None;
    let mut stream: Option<UnixStream> = None;
    let mut retry_stream_at = Instant::now();

    loop {
        let mut request = match pending.take() {
            Some(request) => request,
            None => match rx.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };

        // Preserve byte order while folding a queued wheel/key burst into a
        // single host frame. A session switch is an ordering boundary.
        while request.data.len() < INPUT_BATCH_MAX_BYTES {
            let Ok(next) = rx.try_recv() else {
                break;
            };
            if next.session_dir == request.session_dir
                && request.data.len().saturating_add(next.data.len()) <= INPUT_BATCH_MAX_BYTES
            {
                request.data.extend_from_slice(&next.data);
            } else {
                pending = Some(next);
                break;
            }
        }

        if active_dir.as_ref() != Some(&request.session_dir) {
            active_dir = Some(request.session_dir.clone());
            stream = None;
            retry_stream_at = Instant::now();
        }

        let mut sent_while_connecting = false;
        if stream.is_none() && Instant::now() >= retry_stream_at {
            match connect_input_stream(&request.session_dir, &request.data) {
                Ok(connected) => {
                    stream = Some(connected);
                    sent_while_connecting = true;
                }
                Err(_) => retry_stream_at = Instant::now() + INPUT_STREAM_RETRY_DELAY,
            }
        }

        if sent_while_connecting {
            continue;
        }

        let streamed = stream
            .as_mut()
            .is_some_and(|writer| write_input_frame(writer, &request.data).is_ok());
        if streamed {
            continue;
        }

        // A once-live stream can disappear when its host restarts. Let the
        // next event reconnect immediately; deliver this one through the
        // old one-command protocol so input is not lost during the handoff.
        if stream.take().is_some() {
            retry_stream_at = Instant::now();
        }
        if let Ok(data) = std::str::from_utf8(&request.data) {
            let _ = send_text(&request.session_dir, data);
        }
    }
}

fn roundtrip(session_dir: &Path, command: serde_json::Value) -> Result<serde_json::Value, String> {
    let socket_path = session_dir.join("session.sock");
    let mut stream = UnixStream::connect(&socket_path)
        .map_err(|e| format!("connect {}: {e}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut line = serde_json::to_string(&command).map_err(|e| e.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| format!("read: {e}"))?;
    let parsed: serde_json::Value =
        serde_json::from_str(response.trim()).map_err(|e| format!("bad response: {e}"))?;
    if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(parsed)
    } else {
        Err(parsed
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("host rejected command")
            .to_string())
    }
}

/// Write bytes (control characters included) into the focused session's PTY.
pub fn send_text(session_dir: &Path, data: &str) -> Result<(), String> {
    roundtrip(
        session_dir,
        serde_json::json!({"type": "write", "data": data}),
    )
    .map(|_| ())
}

/// Resize the real PTY — explicit user intent (the focused pane takes the
/// grid, like a phone attach). Every other viewer letterboxes in response.
pub fn send_resize(session_dir: &Path, cols: u16, rows: u16) -> Result<(), String> {
    roundtrip(
        session_dir,
        serde_json::json!({"type": "resize", "cols": cols, "rows": rows}),
    )
    .map(|_| ())
}

pub fn viewport_snapshot(
    session_dir: &Path,
    scroll_offset_rows: u32,
) -> Result<TerminalViewportSnapshot, String> {
    let parsed = roundtrip(
        session_dir,
        serde_json::json!({
            "type": "viewport_snapshot",
            "cols": 0,
            "rows": 0,
            "scroll_offset_rows": scroll_offset_rows,
        }),
    )?;
    let viewport = parsed
        .get("viewport")
        .cloned()
        .ok_or("no viewport in response")?;
    serde_json::from_value(viewport).map_err(|e| format!("bad viewport: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn interactive_input_uses_the_persistent_host_stream() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Keep the Unix socket below macOS's ~104-byte sockaddr_un cap.
        let dir = PathBuf::from(format!("/tmp/uti-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("session.sock")).unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();

        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            let mut writer = stream;
            writer.write_all(&[INPUT_STREAM_ACK]).unwrap();

            let mut received = Vec::new();
            while received.len() < 6 {
                let mut header = [0u8; 4];
                reader.read_exact(&mut header).unwrap();
                let len = u32::from_be_bytes(header) as usize;
                let start = received.len();
                received.resize(start + len, 0);
                reader.read_exact(&mut received[start..]).unwrap();
            }
            seen_tx.send((command, received)).unwrap();
        });

        let input = InteractiveInput::new();
        input.send(&dir, b"abc").unwrap();
        input.send(&dir, b"def").unwrap();

        let (command, received) = seen_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(command, "{\"type\":\"stream_input\"}\n");
        assert_eq!(received, b"abcdef");
        drop(input);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn interactive_input_preframes_first_batch_before_acknowledgement() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = PathBuf::from(format!("/tmp/uti-pre-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("session.sock")).unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();

            // Reproduce an already-running macOS host from before accepted
            // sockets were reset to blocking. If the client waits for this
            // ACK before sending its first frame, the immediate read below
            // deterministically returns WouldBlock and the handler closes.
            stream.set_nonblocking(true).unwrap();
            stream.write_all(&[INPUT_STREAM_ACK]).unwrap();
            let mut header = [0u8; 4];
            reader.read_exact(&mut header).unwrap();
            let mut received = vec![0u8; u32::from_be_bytes(header) as usize];
            reader.read_exact(&mut received).unwrap();
            seen_tx.send((command, received)).unwrap();
        });

        let input = InteractiveInput::new();
        input.send(&dir, b"first key batch").unwrap();

        let (command, received) = seen_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(command, "{\"type\":\"stream_input\"}\n");
        assert_eq!(received, b"first key batch");
        drop(input);
        let _ = fs::remove_dir_all(dir);
    }
}
