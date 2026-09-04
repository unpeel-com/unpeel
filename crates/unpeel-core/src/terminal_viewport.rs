//! Terminal viewport rendering over libghostty-vt.
//!
//! The host keeps a live parsed viewport per session (see `session_host`) and
//! renders one-shot replays for remote clients. Both paths run on the vendored
//! libghostty-vt engine (`vendor/ghostty-vt/`) — the exact same VT
//! implementation that renders the terminal on desktop and phone (GhosttyKit)
//! — so `read_screen`, `wait_for_text`, and menu-prompt detection see the
//! screen the user sees, not an approximation. The previous hand-rolled
//! parser lived here until 2026-07-09.
//!
//! The JSON snapshot shape (`TerminalViewportSnapshot`) is a wire contract
//! with remote clients and the `__viewport__` CLI; keep it stable.

use crate::ghostty_vt as vt;
use crate::session_host::{output_path, read_output_chunk, request_terminal_viewport_snapshot};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const DEFAULT_VIEWPORT_REPLAY_MAX_BYTES: usize = 512 * 1024;
const MAX_VIEWPORT_REPLAY_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Scrollback page-storage budget per virtual terminal. ghostty's
/// `max_scrollback` is in **bytes** (rounded up to page granularity; the
/// header comment saying "lines" is an upstream doc bug — see
/// `Screen.zig`), so this bounds memory directly. Replaces the old
/// parser's 2000-row cap; at typical widths this retains a few hundred to a
/// couple of thousand rows of history.
///
/// This grid is the Host-side *screen* (menu detection, `read_screen`,
/// remote previews), not the durable history: `output.bin` is the journal
/// and `unpeel logs` reads that. The budget was 4 MiB until 2026-09-02,
/// which cost ~6 MiB of resident memory per filled session; every consumer
/// clamps `scroll_offset_rows` to the rows actually retained
/// (`snapshot_terminal`), so a smaller budget only shortens what
/// scrolling-up can reach — it never errors.
const MAX_VIEWPORT_SCROLLBACK_BYTES: usize = 256 * 1024;

/// Bytes of raw output retained per live viewport so `snapshot_resized` can
/// re-render at a different grid size with real reflow (ghostty reflow is not
/// reversible in place, so resized snapshots replay into a fresh terminal).
const RESIZE_REPLAY_MAX_BYTES: usize = DEFAULT_VIEWPORT_REPLAY_MAX_BYTES;

/// Nominal cell pixel size passed to ghostty resize (only image protocols and
/// size reports observe it; text rendering does not).
const CELL_WIDTH_PX: u32 = 8;
const CELL_HEIGHT_PX: u32 = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalViewportStyleRun {
    pub start: u16,
    pub len: u16,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub inverse: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalViewportRow {
    pub text: String,
    pub styles: Vec<TerminalViewportStyleRun>,
    /// This physical row continues on the next row without a hard newline.
    /// Clipboard extraction uses it to unwrap terminal line wrapping.
    #[serde(default)]
    pub wrapped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalViewportSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub output_offset: u64,
    pub truncated: bool,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub scrollback_rows: u32,
    pub viewport_start_row: u32,
    pub scroll_offset_rows: u32,
    /// False only when decoding a snapshot from a pre-input-modes host.
    #[serde(default)]
    pub input_modes_known: bool,
    /// Whether the child currently owns mouse button reports (DEC 9, 1000,
    /// 1002, or 1003). TUI clients use this to distinguish a clickable
    /// full-screen app from ordinary terminal text that should drag-select.
    #[serde(default)]
    pub mouse_reporting: bool,
    #[serde(default)]
    pub mouse_button_motion: bool,
    #[serde(default)]
    pub mouse_any_motion: bool,
    /// Input modes needed to route wheel gestures like a real terminal.
    #[serde(default)]
    pub alternate_screen: bool,
    #[serde(default)]
    pub mouse_alternate_scroll: bool,
    #[serde(default)]
    pub application_cursor: bool,
    pub viewport_rows: Vec<TerminalViewportRow>,
}

/// DEC private modes that shape input semantics and are worth re-asserting
/// after an attach replay reset: application cursor keys (1), mouse tracking
/// (9/1000/1002/1003) and encodings (1005/1006/1015), focus reports (1004),
/// and bracketed paste (2004). All default off.
pub const RESTORABLE_DEC_MODES_DEFAULT_OFF: [u16; 10] =
    [1, 9, 1000, 1002, 1003, 1004, 1005, 1006, 1015, 2004];
/// Default-on DEC private modes a workload may have turned off: autowrap (7)
/// and cursor visibility (25).
pub const RESTORABLE_DEC_MODES_DEFAULT_ON: [u16; 2] = [7, 25];

/// Terminal mode flags a renderer must re-assert after resetting its VT and
/// replaying only an output tail — the sequences that established them
/// usually precede the tail. Published in the session manifest by the host's
/// viewport scan; `set`/`reset` list DEC private mode numbers that differ
/// from their reset defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalModeState {
    #[serde(default)]
    pub alt_screen: bool,
    #[serde(default)]
    pub set: Vec<u16>,
    #[serde(default)]
    pub reset: Vec<u16>,
}

impl TerminalModeState {
    /// True when every flag matches a freshly reset terminal — the manifest
    /// omits the field entirely in that state.
    pub fn is_default(&self) -> bool {
        !self.alt_screen && self.set.is_empty() && self.reset.is_empty()
    }

    /// The escape preamble that re-establishes these modes on a freshly
    /// reset terminal. Alt screen first, so the tail replay draws into the
    /// screen the workload considers active.
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

/// The Host's resident VT state as a VT byte sequence: fed into a freshly
/// reset terminal of the same `cols`×`rows`, it reproduces the active
/// screen's cells (text, wide characters and grapheme clusters, every SGR
/// attribute and 16/256/truecolor fg/bg/underline colors), the resident
/// scrollback rows above it, every mode that differs from its default
/// (alternate screen, cursor visibility, autowrap, origin, bracketed paste,
/// mouse reporting and encodings, focus events, application cursor keys,
/// synchronized output …), the scrolling region, tabstops, charsets, the
/// Kitty keyboard flags, the cursor's active pen, and finally the cursor
/// position. Rendered by libghostty-vt's formatter (`terminal/formatter.zig`
/// upstream), so it is the same engine's own view of its state.
///
/// Not reproduced: the palette and OSC 7 (the client terminal keeps its own
/// theme and cwd), the cursor *shape* (DECSCUSR is not exposed by the
/// library), kitty graphics (the Host strips them before parsing), and the
/// pending-wrap flag of a cursor parked past the last column (the next
/// printed character lands on that column instead of wrapping; the very
/// next `\r`/`\n`/CUP from the live stream resynchronizes it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotVt {
    pub cols: u16,
    pub rows: u16,
    #[serde(with = "serde_bytes_vec")]
    pub bytes: Vec<u8>,
}

/// `Vec<u8>` as a plain JSON byte array is what serde does anyway; this
/// module exists so the wire shape is pinned explicitly (a future base64
/// encoding must be a new field, never a silent change).
mod serde_bytes_vec {
    pub fn serialize<S: serde::Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(bytes, s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        serde::Deserialize::deserialize(d)
    }
}

/// The 1-based top and left margins the formatter emitted (`CSI t;b r`,
/// `CSI l;r s`); 1 when a margin sequence is absent (full screen).
fn margins_from_vt(bytes: &[u8]) -> (u32, u32) {
    fn first_param(bytes: &[u8], terminator: u8) -> Option<u32> {
        let mut last = None;
        let mut i = 0;
        while i + 2 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == terminator {
                    let params = &bytes[i + 2..j];
                    let first = params.split(|&b| b == b';').next().unwrap_or(&[]);
                    if let Ok(value) = std::str::from_utf8(first).unwrap_or("").parse::<u32>() {
                        last = Some(value.max(1));
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
        last
    }
    (
        first_param(bytes, b'r').unwrap_or(1),
        first_param(bytes, b's').unwrap_or(1),
    )
}

/// Render `state`'s resident VT into a [`SnapshotVt`]. The journal offset the
/// snapshot corresponds to is `state.output_offset()`; callers that need both
/// atomically use [`TerminalViewportState::snapshot_vt`], which reads them
/// under the same borrow.
pub fn render_snapshot_vt(state: &TerminalViewportState) -> SnapshotVt {
    SnapshotVt {
        cols: state.term.cols,
        rows: state.term.rows,
        bytes: state.term.render_snapshot_vt(),
    }
}

/// Per-cell style in the snapshot's string encoding: `ansi:N` (palette 0-15),
/// `ansi256:N`, or `rgb:r,g,b`. Matches what the old parser emitted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CellStyle {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    inverse: bool,
}

/// Owned libghostty-vt terminal handle for one virtual terminal. Not
/// thread-safe by itself; every user wraps it in a `Mutex` (the live
/// per-session viewport, the replay cache).
///
/// Rendering scratch (the libghostty `RenderState` with its per-row cell
/// cache, the row iterator, the cells cursor) is deliberately NOT owned here:
/// a RenderState keeps `rows × cols × 64 B` resident (125 KiB at 80×24,
/// 384 KiB at 200 columns) from the first update on, which was the largest
/// non-VT cost of every idle Session in the PTY core. Snapshots go through
/// [`with_render_scratch`], one scratch per rendering thread.
struct VtTerminal {
    term: vt::GhosttyTerminal,
    /// Process-unique identity for render-scratch binding. A pointer would
    /// do until a dropped terminal's address is reused by a new one, at
    /// which point a stale incremental render state would be trusted.
    render_id: u64,
    cols: u16,
    rows: u16,
}

// The raw handles are only ever touched through &mut self (or &self getters
// that ghostty documents as pure reads), and all shared use is Mutex-guarded.
unsafe impl Send for VtTerminal {}

impl Drop for VtTerminal {
    fn drop(&mut self) {
        unsafe {
            vt::ghostty_terminal_free(self.term);
        }
    }
}

static NEXT_RENDER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One thread's libghostty render scratch, rebound to whichever terminal is
/// being snapshotted. `ghostty_render_state_update` is incremental: it
/// consumes the terminal's dirty flags and keeps the rows it already has,
/// so a state that last rendered a *different* terminal must not be reused
/// as-is — the previous terminal's rows would survive wherever the new one
/// is not dirty. Binding a different terminal therefore drops and recreates
/// the RenderState (its row cache is exactly the memory being reclaimed);
/// consecutive snapshots of the same terminal keep the incremental path.
struct RenderScratch {
    render: vt::GhosttyRenderState,
    row_iter: vt::GhosttyRenderStateRowIterator,
    cells: vt::GhosttyRenderStateRowCells,
    bound: u64,
}

impl RenderScratch {
    fn new() -> Self {
        unsafe {
            let mut row_iter: vt::GhosttyRenderStateRowIterator = std::ptr::null_mut();
            assert_eq!(
                vt::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut row_iter),
                vt::GHOSTTY_SUCCESS,
                "libghostty-vt row iterator allocation failed"
            );
            let mut cells: vt::GhosttyRenderStateRowCells = std::ptr::null_mut();
            assert_eq!(
                vt::ghostty_render_state_row_cells_new(std::ptr::null(), &mut cells),
                vt::GHOSTTY_SUCCESS,
                "libghostty-vt row cells allocation failed"
            );
            Self {
                render: Self::new_render_state(),
                row_iter,
                cells,
                bound: 0,
            }
        }
    }

    unsafe fn new_render_state() -> vt::GhosttyRenderState {
        let mut render: vt::GhosttyRenderState = std::ptr::null_mut();
        assert_eq!(
            vt::ghostty_render_state_new(std::ptr::null(), &mut render),
            vt::GHOSTTY_SUCCESS,
            "libghostty-vt render state allocation failed"
        );
        render
    }

    /// Bind to `term` and bring the render state up to date with it.
    fn bind(&mut self, term: &VtTerminal) {
        unsafe {
            if self.bound != term.render_id {
                vt::ghostty_render_state_free(self.render);
                self.render = Self::new_render_state();
                self.bound = term.render_id;
            }
            vt::ghostty_render_state_update(self.render, term.term);
        }
    }

    /// Drop the row cache while keeping the (tiny) iterator handles, so an
    /// idle rendering thread holds nothing grid-sized.
    fn release(&mut self) {
        unsafe {
            vt::ghostty_render_state_free(self.render);
            self.render = Self::new_render_state();
            self.bound = 0;
        }
    }
}

impl Drop for RenderScratch {
    fn drop(&mut self) {
        unsafe {
            vt::ghostty_render_state_row_cells_free(self.cells);
            vt::ghostty_render_state_row_iterator_free(self.row_iter);
            vt::ghostty_render_state_free(self.render);
        }
    }
}

thread_local! {
    static RENDER_SCRATCH: std::cell::RefCell<Option<RenderScratch>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with this thread's render scratch bound to `term` (created on
/// first use, updated to the terminal's current state). The scratch lives
/// for the thread; there are a handful of rendering threads per Host (the
/// reactor, the timer, control-socket clients), never one per Session.
fn with_render_scratch<R>(term: &VtTerminal, f: impl FnOnce(&mut RenderScratch) -> R) -> R {
    RENDER_SCRATCH.with(|slot| {
        let mut slot = slot.borrow_mut();
        let scratch = slot.get_or_insert_with(RenderScratch::new);
        scratch.bind(term);
        f(scratch)
    })
}

/// Free this thread's render row cache (the scratch itself stays for reuse).
/// Called from the same idle release path as `release_memory_to_os` so a
/// thread that stopped snapshotting holds no grid-sized buffers.
pub fn release_render_scratch() {
    RENDER_SCRATCH.with(|slot| {
        if let Some(scratch) = slot.borrow_mut().as_mut() {
            scratch.release();
        }
    });
}

impl VtTerminal {
    fn new(cols: u16, rows: u16) -> Self {
        Self::with_scrollback(cols, rows, MAX_VIEWPORT_SCROLLBACK_BYTES)
    }

    /// A terminal with an explicit scrollback byte budget. Resident
    /// per-session grids use `new`; short-lived scratch terminals that replay
    /// the on-disk journal for deep scrolling get a larger budget because
    /// they are dropped as soon as the snapshot is taken.
    fn with_scrollback(cols: u16, rows: u16, max_scrollback: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let options = vt::GhosttyTerminalOptions {
            cols,
            rows,
            max_scrollback,
        };
        unsafe {
            let mut term: vt::GhosttyTerminal = std::ptr::null_mut();
            assert_eq!(
                vt::ghostty_terminal_new(std::ptr::null(), &mut term, options),
                vt::GHOSTTY_SUCCESS,
                "libghostty-vt terminal allocation failed"
            );
            Self {
                term,
                render_id: NEXT_RENDER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                cols,
                rows,
            }
        }
    }

    fn write(&mut self, data: &[u8]) {
        unsafe { vt::ghostty_terminal_vt_write(self.term, data.as_ptr(), data.len()) }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        unsafe {
            vt::ghostty_terminal_resize(self.term, cols, rows, CELL_WIDTH_PX, CELL_HEIGHT_PX);
        }
        self.cols = cols;
        self.rows = rows;
    }

    fn get_usize(&self, data: std::os::raw::c_int) -> usize {
        let mut out: usize = 0;
        unsafe {
            vt::ghostty_terminal_get(self.term, data, &mut out as *mut usize as *mut _);
        }
        out
    }

    fn get_u16(&self, data: std::os::raw::c_int) -> u16 {
        let mut out: u16 = 0;
        unsafe {
            vt::ghostty_terminal_get(self.term, data, &mut out as *mut u16 as *mut _);
        }
        out
    }

    fn scrollback_rows(&self) -> usize {
        self.get_usize(vt::GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS)
    }

    fn cursor(&self) -> (u16, u16) {
        (
            self.get_u16(vt::GHOSTTY_TERMINAL_DATA_CURSOR_X),
            self.get_u16(vt::GHOSTTY_TERMINAL_DATA_CURSOR_Y),
        )
    }

    /// True while the fed stream sits inside a DEC 2026 synchronized update
    /// (`?2026h` seen, `?2026l` not yet) — the app has declared the grid a
    /// work-in-progress. Ghostty tracks the mode; unknown-mode errors read
    /// as false.
    fn synchronized_output(&self) -> bool {
        let mut value = false;
        let rc = unsafe { vt::ghostty_terminal_mode_get(self.term, 2026, &mut value) };
        rc == vt::GHOSTTY_SUCCESS && value
    }

    /// DEC mouse tracking modes. Encoding modes such as 1006 only describe
    /// reports; one of these tracking modes must be active for the child to
    /// own button gestures.
    fn mouse_reporting(&self) -> bool {
        [9, 1000, 1002, 1003].into_iter().any(|mode| {
            let mut value = false;
            let rc = unsafe { vt::ghostty_terminal_mode_get(self.term, mode, &mut value) };
            rc == vt::GHOSTTY_SUCCESS && value
        })
    }

    fn mode(&self, mode: u16) -> bool {
        let mut value = false;
        let rc = unsafe { vt::ghostty_terminal_mode_get(self.term, mode, &mut value) };
        rc == vt::GHOSTTY_SUCCESS && value
    }

    #[cfg(test)]
    fn cursor_pending_wrap(&self) -> bool {
        let mut value = false;
        let rc = unsafe {
            vt::ghostty_terminal_get(
                self.term,
                vt::GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP,
                &mut value as *mut _ as *mut _,
            )
        };
        rc == vt::GHOSTTY_SUCCESS && value
    }

    /// Serialize the active screen (scrollback included) plus the terminal
    /// state that shapes rendering and input into a VT byte sequence, via
    /// libghostty-vt's own formatter. See [`SnapshotVt`] for what is and is
    /// not reproduced.
    fn render_snapshot_vt(&self) -> Vec<u8> {
        let mut options = vt::GhosttyFormatterTerminalOptions::vt_state_snapshot();
        // Soft-wrapped rows are re-joined so the receiving terminal re-wraps
        // them itself and keeps the wrap flags — but only while autowrap is
        // on there; with DECAWM off (emitted before the content) a joined
        // row would be clipped at the last column instead.
        options.unwrap = self.mode(7);
        unsafe {
            let mut formatter: vt::GhosttyFormatter = std::ptr::null_mut();
            if vt::ghostty_formatter_terminal_new(
                std::ptr::null(),
                &mut formatter,
                self.term,
                options,
            ) != vt::GHOSTTY_SUCCESS
            {
                return Vec::new();
            }
            let mut needed: usize = 0;
            let rc =
                vt::ghostty_formatter_format_buf(formatter, std::ptr::null_mut(), 0, &mut needed);
            let mut out = Vec::new();
            if rc == vt::GHOSTTY_OUT_OF_SPACE && needed > 0 {
                out = vec![0u8; needed];
                let mut written: usize = 0;
                if vt::ghostty_formatter_format_buf(
                    formatter,
                    out.as_mut_ptr(),
                    out.len(),
                    &mut written,
                ) == vt::GHOSTTY_SUCCESS
                {
                    out.truncate(written);
                } else {
                    out.clear();
                }
            }
            vt::ghostty_formatter_free(formatter);
            // The formatter parks the cursor with CUP *before* its trailing
            // terminal extras, and both DECSTBM and the tabstop block (CSI G
            // + HTS per stop) move the cursor again. Re-park it last. Under
            // origin mode CUP is relative to the margins the same output just
            // established, so translate to region-relative coordinates.
            let (x, y) = self.cursor();
            let (mut row, mut col) = (u32::from(y) + 1, u32::from(x) + 1);
            if self.mode(6) {
                let (top, left) = margins_from_vt(&out);
                row = row.saturating_sub(top - 1).max(1);
                col = col.saturating_sub(left - 1).max(1);
            }
            out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
            out
        }
    }

    fn alternate_screen(&self) -> bool {
        let mut screen = vt::GHOSTTY_TERMINAL_SCREEN_PRIMARY;
        let rc = unsafe {
            vt::ghostty_terminal_get(
                self.term,
                vt::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
                &mut screen as *mut _ as *mut _,
            )
        };
        rc == vt::GHOSTTY_SUCCESS && screen == vt::GHOSTTY_TERMINAL_SCREEN_ALTERNATE
    }

    fn scroll_to_row(&mut self, row: usize) {
        unsafe {
            vt::ghostty_terminal_scroll_viewport(
                self.term,
                vt::GhosttyTerminalScrollViewport::row(row),
            );
        }
    }

    fn scroll_bottom(&mut self) {
        unsafe {
            vt::ghostty_terminal_scroll_viewport(
                self.term,
                vt::GhosttyTerminalScrollViewport::bottom(),
            );
        }
    }

    /// Render the current viewport and return its rows.
    fn viewport_rows(&mut self, with_styles: bool) -> Vec<TerminalViewportRow> {
        let row_count = self.rows as usize;
        with_render_scratch(self, |scratch| unsafe {
            if vt::ghostty_render_state_get(
                scratch.render,
                vt::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                &mut scratch.row_iter as *mut _ as *mut _,
            ) != vt::GHOSTTY_SUCCESS
            {
                return Vec::new();
            }

            let mut rows = Vec::with_capacity(row_count);
            while vt::ghostty_render_state_row_iterator_next(scratch.row_iter) {
                let mut wrapped = false;
                let mut raw_row: vt::GhosttyRow = 0;
                if vt::ghostty_render_state_row_get(
                    scratch.row_iter,
                    vt::GHOSTTY_RENDER_STATE_ROW_DATA_RAW,
                    &mut raw_row as *mut _ as *mut _,
                ) == vt::GHOSTTY_SUCCESS
                {
                    let _ = vt::ghostty_row_get(
                        raw_row,
                        vt::GHOSTTY_ROW_DATA_WRAP,
                        &mut wrapped as *mut _ as *mut _,
                    );
                }
                if vt::ghostty_render_state_row_get(
                    scratch.row_iter,
                    vt::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                    &mut scratch.cells as *mut _ as *mut _,
                ) != vt::GHOSTTY_SUCCESS
                {
                    rows.push(TerminalViewportRow {
                        text: String::new(),
                        styles: Vec::new(),
                        wrapped,
                    });
                    continue;
                }
                let mut row = read_row_cells(scratch.cells, with_styles);
                row.wrapped = wrapped;
                rows.push(row);
            }
            rows
        })
    }
}

/// One rendered cell: its text piece (empty for wide-char spacers, a space
/// for blank cells) and its style.
fn read_cell(
    cells: vt::GhosttyRenderStateRowCells,
    with_styles: bool,
    previous_style: &CellStyle,
) -> (String, CellStyle) {
    unsafe {
        let mut buf = [0u8; 64];
        let mut gbuf = vt::GhosttyBuffer {
            ptr: buf.as_mut_ptr(),
            cap: buf.len(),
            len: 0,
        };
        let result = vt::ghostty_render_state_row_cells_get(
            cells,
            vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
            &mut gbuf as *mut _ as *mut _,
        );

        let text = if result == vt::GHOSTTY_SUCCESS && gbuf.len > 0 {
            String::from_utf8_lossy(&buf[..gbuf.len]).into_owned()
        } else if result == vt::GHOSTTY_OUT_OF_SPACE {
            // Oversized grapheme cluster; retry with a heap buffer.
            let mut heap = vec![0u8; gbuf.len.max(64)];
            let mut retry = vt::GhosttyBuffer {
                ptr: heap.as_mut_ptr(),
                cap: heap.len(),
                len: 0,
            };
            if vt::ghostty_render_state_row_cells_get(
                cells,
                vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8,
                &mut retry as *mut _ as *mut _,
            ) == vt::GHOSTTY_SUCCESS
            {
                String::from_utf8_lossy(&heap[..retry.len]).into_owned()
            } else {
                " ".to_string()
            }
        } else {
            // No text: blank cell, or the spacer of a wide character. The
            // spacer must not add a column of text; wide-char continuation
            // keeps the head cell's style so style runs span both columns.
            let mut raw: vt::GhosttyCell = 0;
            let mut wide: std::os::raw::c_int = 0;
            if vt::ghostty_render_state_row_cells_get(
                cells,
                vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                &mut raw as *mut _ as *mut _,
            ) == vt::GHOSTTY_SUCCESS
                && vt::ghostty_cell_get(
                    raw,
                    vt::GHOSTTY_CELL_DATA_WIDE,
                    &mut wide as *mut _ as *mut _,
                ) == vt::GHOSTTY_SUCCESS
                && (wide == vt::GHOSTTY_CELL_WIDE_SPACER_TAIL
                    || wide == vt::GHOSTTY_CELL_WIDE_SPACER_HEAD)
            {
                return (String::new(), previous_style.clone());
            }
            " ".to_string()
        };

        if !with_styles {
            return (text, CellStyle::default());
        }

        let mut cell_style = CellStyle::default();
        let mut has_styling = false;
        if vt::ghostty_render_state_row_cells_get(
            cells,
            vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
            &mut has_styling as *mut _ as *mut _,
        ) == vt::GHOSTTY_SUCCESS
            && has_styling
        {
            let mut style = vt::GhosttyStyle::zeroed_sized();
            if vt::ghostty_render_state_row_cells_get(
                cells,
                vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                &mut style as *mut _ as *mut _,
            ) == vt::GHOSTTY_SUCCESS
            {
                cell_style = CellStyle {
                    fg: style_color_string(&style.fg_color),
                    bg: style_color_string(&style.bg_color),
                    bold: style.bold,
                    inverse: style.inverse,
                };
            }
        }

        // EL/ECH store their SGR background directly on each erased cell's
        // content tag, not in its style. `HAS_STYLING` is therefore false for
        // the blank tail even though Ghostty renders it with a background.
        // Ask the resolved-color API when the explicit style had no bg so
        // full-width TUI fills survive viewport serialization.
        if cell_style.bg.is_none() {
            let mut rgb = vt::GhosttyColorRgb::default();
            if vt::ghostty_render_state_row_cells_get(
                cells,
                vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                &mut rgb as *mut _ as *mut _,
            ) == vt::GHOSTTY_SUCCESS
            {
                cell_style.bg = Some(format!("rgb:{},{},{}", rgb.r, rgb.g, rgb.b));
            }
        }

        (text, cell_style)
    }
}

fn style_color_string(color: &vt::GhosttyStyleColor) -> Option<String> {
    match color.tag {
        vt::GHOSTTY_STYLE_COLOR_PALETTE => {
            let index = unsafe { color.value.palette };
            if index < 16 {
                Some(format!("ansi:{index}"))
            } else {
                Some(format!("ansi256:{index}"))
            }
        }
        vt::GHOSTTY_STYLE_COLOR_RGB => {
            let rgb = unsafe { color.value.rgb };
            Some(format!("rgb:{},{},{}", rgb.r, rgb.g, rgb.b))
        }
        _ => None,
    }
}

fn read_row_cells(cells: vt::GhosttyRenderStateRowCells, with_styles: bool) -> TerminalViewportRow {
    let mut pieces: Vec<String> = Vec::new();
    let mut cell_styles: Vec<CellStyle> = Vec::new();
    let mut previous_style = CellStyle::default();

    while unsafe { vt::ghostty_render_state_row_cells_next(cells) } {
        let (text, style) = read_cell(cells, with_styles, &previous_style);
        previous_style = style.clone();
        pieces.push(text);
        cell_styles.push(style);
    }

    // Trailing trim mirrors the old parser: drop trailing blank cells (and
    // any spacer pieces) from the text payload, keep everything before.
    let text_end = pieces
        .iter()
        .rposition(|piece| !piece.is_empty() && piece != " ")
        .map(|index| index + 1)
        .unwrap_or(0);
    let text: String = pieces[..text_end].concat();

    let styles = if with_styles {
        style_runs(&cell_styles)
    } else {
        Vec::new()
    };
    TerminalViewportRow {
        text,
        styles,
        wrapped: false,
    }
}

fn style_runs(cell_styles: &[CellStyle]) -> Vec<TerminalViewportStyleRun> {
    let default_style = CellStyle::default();
    let mut styles = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut run_style: Option<&CellStyle> = None;

    for (index, style) in cell_styles.iter().enumerate() {
        if style == &default_style {
            if let (Some(start), Some(style)) = (run_start.take(), run_style.take()) {
                styles.push(viewport_style_run(start, index - start, style));
            }
            continue;
        }

        if run_style.is_some_and(|run| run == style) {
            continue;
        }

        if let (Some(start), Some(style)) = (run_start.replace(index), run_style.replace(style)) {
            styles.push(viewport_style_run(start, index - start, style));
        }
    }

    if let (Some(start), Some(style)) = (run_start, run_style) {
        styles.push(viewport_style_run(start, cell_styles.len() - start, style));
    }

    styles
}

fn viewport_style_run(start: usize, len: usize, style: &CellStyle) -> TerminalViewportStyleRun {
    TerminalViewportStyleRun {
        start: start.min(u16::MAX as usize) as u16,
        len: len.min(u16::MAX as usize) as u16,
        fg: style.fg.clone(),
        bg: style.bg.clone(),
        bold: style.bold,
        inverse: style.inverse,
    }
}

/// Render a snapshot window from a terminal. Scrolls the terminal viewport to
/// cover the requested window (possibly in multiple passes when the window is
/// taller than the grid) and pins it back to the bottom afterwards.
fn snapshot_terminal(
    term: &mut VtTerminal,
    output_offset: u64,
    truncated: bool,
    scroll_offset_rows: u32,
    viewport_row_count: Option<u16>,
) -> TerminalViewportSnapshot {
    let grid_rows = term.rows as usize;
    let grid_cols = term.cols;
    let scrollback_rows = term.scrollback_rows();
    let total_rows = scrollback_rows + grid_rows;
    let viewport_row_count = viewport_row_count
        .map(|value| value.max(1) as usize)
        .unwrap_or(grid_rows)
        .min(total_rows.max(1));
    let max_scroll_offset = total_rows.saturating_sub(viewport_row_count) as u32;
    let scroll_offset_rows = scroll_offset_rows.min(max_scroll_offset);
    let viewport_start_row = total_rows
        .saturating_sub(viewport_row_count)
        .saturating_sub(scroll_offset_rows as usize);

    let end_row = viewport_start_row + viewport_row_count;
    let mut viewport_rows = Vec::with_capacity(viewport_row_count);
    let mut absolute_row = viewport_start_row;
    while absolute_row < end_row {
        // Any absolute row is reachable from a viewport position in
        // [0, scrollback_rows]; skip inside the rendered pass when the
        // position had to clamp.
        let position = absolute_row.min(scrollback_rows);
        term.scroll_to_row(position);
        let skip = absolute_row - position;
        let rendered = term.viewport_rows(true);
        if skip >= rendered.len() {
            break;
        }
        for row in rendered.into_iter().skip(skip) {
            if absolute_row >= end_row {
                break;
            }
            viewport_rows.push(row);
            absolute_row += 1;
        }
    }
    term.scroll_bottom();

    let (cursor_col, cursor_row) = term.cursor();
    TerminalViewportSnapshot {
        cols: grid_cols,
        rows: grid_rows as u16,
        output_offset,
        truncated,
        cursor_row: cursor_row.min((grid_rows as u16).saturating_sub(1)),
        cursor_col: cursor_col.min(grid_cols.saturating_sub(1)),
        scrollback_rows: scrollback_rows as u32,
        viewport_start_row: viewport_start_row as u32,
        scroll_offset_rows,
        input_modes_known: true,
        mouse_reporting: term.mouse_reporting(),
        mouse_button_motion: term.mode(1002) || term.mode(1003),
        mouse_any_motion: term.mode(1003),
        alternate_screen: term.alternate_screen(),
        mouse_alternate_scroll: term.mode(1007),
        application_cursor: term.mode(1),
        viewport_rows,
    }
}

/// Hand freed heap back to the OS. Dropping a session's VT, journal batch
/// buffers, and client backlogs returns them to malloc, but the allocator
/// keeps the pages: after 50 filled sessions closed, the PTY core sat at
/// ~22 MiB phys_footprint with zero sessions (2026-09-02). Session teardown
/// in the core calls this once; it is cheap (microseconds when there is
/// nothing to give back) and safe from any thread.
pub fn release_memory_to_os() {
    // The calling thread's render row cache is grid-sized and only useful
    // while it keeps snapshotting the same terminal; give it back first so
    // the allocator release below can hand its pages to the OS too.
    release_render_scratch();
    // With the mimalloc global allocator (unpeel-host `--features mimalloc`)
    // the libmalloc pressure-relief call below only sees the zones mimalloc
    // does not own; `mi_collect(true)` is the equivalent that frees
    // mimalloc's retired segments and purges its free pages to the OS.
    #[cfg(feature = "mimalloc")]
    unsafe {
        libmimalloc_sys::mi_collect(true);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        extern "C" {
            // malloc/malloc.h: returns the number of bytes released; a NULL
            // zone means every zone.
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        malloc_trim(0);
    }
}

/// Where a viewport can re-read output it no longer holds in its resident
/// grid: the session's `output.bin` journal. With a journal attached the
/// viewport retains no raw bytes of its own; resized snapshots and scroll
/// offsets beyond the resident budget replay the journal into a scratch
/// terminal instead.
#[derive(Clone, Debug)]
enum JournalSource {
    /// A hosted session: read through `session_host::read_output_chunk`,
    /// which honours the retained-tail marker and escape alignment.
    Session(String),
    /// A bare journal file (tests, tools): the trailing bytes, aligned to
    /// the next line start.
    Path(PathBuf),
}

/// Bytes of journal replayed into a scratch terminal for a scroll offset
/// beyond the resident scrollback. Transient: the scratch terminal is
/// dropped with the snapshot.
const JOURNAL_SCROLL_REPLAY_MAX_BYTES: usize = MAX_VIEWPORT_REPLAY_MAX_BYTES;

pub struct TerminalViewportState {
    term: VtTerminal,
    /// Bounded raw-output tail for `snapshot_resized` replays, kept only
    /// while no journal is attached (remote previews, tests).
    resize_replay: Vec<u8>,
    journal: Option<JournalSource>,
    output_offset: u64,
    history_truncated: bool,
    apc_filter: KittyApcFilter,
    /// Complete kitty APC sequences captured for passthrough, oldest first.
    /// Only populated when `set_graphics_capture(true)`; bounded by
    /// `MAX_CAPTURED_GRAPHICS` / `MAX_CAPTURED_APC_BYTES`.
    captured_graphics: Vec<CapturedGraphicsCommand>,
}

/// One complete kitty graphics APC (`ESC _ G … ESC \`) captured from a fed
/// stream, with the 0-based cursor position the sequence arrived at — the
/// cell a placement anchors to.
#[derive(Clone, Debug)]
pub struct CapturedGraphicsCommand {
    pub bytes: Vec<u8>,
    pub row: u16,
    pub col: u16,
}

/// A single captured APC larger than this is dropped (matching the historic
/// strip behavior) instead of buffered: file-medium graphics are tiny, and
/// kitty clients chunk direct-medium payloads at 4 KiB.
const MAX_CAPTURED_APC_BYTES: usize = 256 * 1024;
/// Captured-but-undrained sequences beyond this drop oldest-first; a live
/// renderer drains every frame, so this only bounds an unattached consumer.
const MAX_CAPTURED_GRAPHICS: usize = 128;

/// Strips kitty graphics sequences (`ESC _ G … ESC \`) from a byte stream,
/// preserving everything else byte-for-byte, stateful across chunk
/// boundaries. Kitty always emits the 7-bit forms, so the 8-bit APC/ST
/// bytes (0x9F/0x9C) are deliberately not handled.
///
/// With `capture` enabled, each complete stripped sequence is surfaced as a
/// `Segment::Graphics` in stream order instead of vanishing, so a renderer
/// can forward it to a real terminal (the TUI's kitty passthrough).
#[derive(Default)]
struct KittyApcFilter {
    state: ApcFilterState,
    capture: bool,
    /// The in-flight APC's bytes (including `ESC _ G`), while capturing.
    pending: Vec<u8>,
    /// The in-flight APC exceeded `MAX_CAPTURED_APC_BYTES` and is dropped.
    oversized: bool,
}

/// One ordered piece of a scanned chunk: pass-through bytes for the VT, or a
/// complete kitty graphics sequence.
enum ScanSegment {
    Text(Vec<u8>),
    Graphics(Vec<u8>),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ApcFilterState {
    #[default]
    Ground,
    /// ESC seen in ground and withheld; the next byte decides.
    Esc,
    /// `ESC _` seen and withheld; `G` makes it a kitty sequence.
    EscUnderscore,
    /// Inside a kitty payload — bytes are dropped.
    Apc,
    /// Inside a kitty payload, ESC seen — `\` (ST) ends the sequence.
    ApcEsc,
}

impl KittyApcFilter {
    /// Returns None when the chunk passes through untouched (the common
    /// zero-copy case), or the ordered text/graphics segments otherwise.
    fn scan(&mut self, data: &[u8]) -> Option<Vec<ScanSegment>> {
        if self.state == ApcFilterState::Ground && !Self::needs_scan(data) {
            return None;
        }
        let mut segments = Vec::new();
        let mut text = Vec::with_capacity(data.len());
        for &byte in data {
            match self.state {
                ApcFilterState::Ground => {
                    if byte == 0x1b {
                        self.state = ApcFilterState::Esc;
                    } else {
                        text.push(byte);
                    }
                }
                ApcFilterState::Esc => {
                    if byte == b'_' {
                        self.state = ApcFilterState::EscUnderscore;
                    } else if byte == 0x1b {
                        // ESC ESC: emit the withheld one, keep waiting.
                        text.push(0x1b);
                    } else {
                        text.push(0x1b);
                        text.push(byte);
                        self.state = ApcFilterState::Ground;
                    }
                }
                ApcFilterState::EscUnderscore => {
                    if byte == b'G' {
                        self.state = ApcFilterState::Apc;
                        if self.capture {
                            self.pending.clear();
                            self.oversized = false;
                            self.pending.extend_from_slice(b"\x1b_G");
                        }
                    } else {
                        // A non-kitty APC (e.g. tmux passthrough): emit the
                        // withheld introducer and continue untouched.
                        text.push(0x1b);
                        text.push(b'_');
                        text.push(byte);
                        self.state = ApcFilterState::Ground;
                    }
                }
                ApcFilterState::Apc => {
                    if byte == 0x1b {
                        self.state = ApcFilterState::ApcEsc;
                    } else if matches!(byte, 0x18 | 0x1a) {
                        // CAN/SUB abort the control string; the byte belongs
                        // to the aborted image sequence and is dropped with it.
                        self.state = ApcFilterState::Ground;
                        self.pending.clear();
                    } else {
                        self.push_pending(byte);
                    }
                }
                ApcFilterState::ApcEsc => {
                    if byte == b'\\' {
                        self.state = ApcFilterState::Ground;
                        if self.capture && !self.oversized && !self.pending.is_empty() {
                            if !text.is_empty() {
                                segments.push(ScanSegment::Text(std::mem::take(&mut text)));
                            }
                            self.pending.extend_from_slice(b"\x1b\\");
                            segments.push(ScanSegment::Graphics(std::mem::take(&mut self.pending)));
                        }
                        self.pending.clear();
                        self.oversized = false;
                    } else if byte != 0x1b {
                        self.state = ApcFilterState::Apc;
                        self.push_pending(0x1b);
                        self.push_pending(byte);
                    } else {
                        self.push_pending(0x1b);
                    }
                }
            }
        }
        if !text.is_empty() {
            segments.push(ScanSegment::Text(text));
        }
        Some(segments)
    }

    /// Test-only flat view of `scan`: text segments concatenated, graphics
    /// dropped — the historic `filter` shape.
    #[cfg(test)]
    fn filter(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        self.scan(data).map(|segments| {
            let mut out = Vec::new();
            for segment in segments {
                if let ScanSegment::Text(bytes) = segment {
                    out.extend_from_slice(&bytes);
                }
            }
            out
        })
    }

    fn push_pending(&mut self, byte: u8) {
        if !self.capture || self.oversized {
            return;
        }
        if self.pending.len() >= MAX_CAPTURED_APC_BYTES {
            self.oversized = true;
            self.pending.clear();
            return;
        }
        self.pending.push(byte);
    }

    /// True when the chunk could start or continue a kitty sequence: an
    /// adjacent `ESC _` pair anywhere, or a trailing ESC / `ESC _` that the
    /// next chunk must resolve.
    fn needs_scan(data: &[u8]) -> bool {
        if data.last() == Some(&0x1b) {
            return true;
        }
        data.windows(2).any(|pair| pair == [0x1b, b'_'])
    }
}

impl TerminalViewportState {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            term: VtTerminal::new(cols, rows),
            resize_replay: Vec::new(),
            journal: None,
            output_offset: 0,
            history_truncated: false,
            apc_filter: KittyApcFilter::default(),
            captured_graphics: Vec::new(),
        }
    }

    /// Absolute byte offset immediately after the output parsed so far.
    pub fn output_offset(&self) -> u64 {
        self.output_offset
    }

    // R3-COORD: Lane 1 stand-in. A core rebuilt from a handoff feeds the
    // snapshot VT bytes (which are not journal bytes) into a fresh terminal
    // and then pins the offset back to the journal position the snapshot
    // was rendered at, so `snapshot_vt` and the broadcaster keep agreeing.
    pub fn set_output_offset(&mut self, output_offset: u64) {
        self.output_offset = output_offset;
    }

    /// Replace the parsed terminal before feeding a rebased output stream.
    ///
    /// Remote renderers use this when the Host cannot continue from the
    /// Controller's committed cursor. `output_offset` is the absolute offset
    /// of the first byte that will be fed after the reset.
    pub fn reset_at_output_offset(
        &mut self,
        cols: u16,
        rows: u16,
        output_offset: u64,
        history_truncated: bool,
    ) {
        let capture = self.apc_filter.capture;
        self.term = VtTerminal::new(cols, rows);
        self.resize_replay.clear();
        self.output_offset = output_offset;
        self.history_truncated = history_truncated;
        self.apc_filter = KittyApcFilter {
            capture,
            ..KittyApcFilter::default()
        };
        self.captured_graphics.clear();
    }

    pub fn feed(&mut self, data: &[u8]) {
        // Kitty graphics payloads (APC `ESC _ G … ESC \`) are stripped
        // before parsing: this viewport exists for text snapshots, menu
        // detection, and mode state — none of which images influence — and
        // an image-heavy workload (terminal-browser --app-mode) streams
        // megabytes of base64 per second through this call, which sits
        // synchronously in the session host's PTY read loop. Parsing and
        // storing those images in the host's VT was pure overhead on the
        // interactive path.
        //
        // With graphics capture enabled (the TUI's kitty passthrough), each
        // stripped sequence is retained in stream order instead, stamped
        // with the cursor position it arrived at: preceding bytes have
        // already been parsed, so the VT cursor is the placement anchor.
        match self.apc_filter.scan(data) {
            None => self.write_terminal(data),
            Some(segments) => {
                for segment in segments {
                    match segment {
                        ScanSegment::Text(bytes) => self.write_terminal(&bytes),
                        ScanSegment::Graphics(bytes) => {
                            let (col, row) = self.term.cursor();
                            if self.captured_graphics.len() >= MAX_CAPTURED_GRAPHICS {
                                self.captured_graphics.remove(0);
                            }
                            self.captured_graphics.push(CapturedGraphicsCommand {
                                bytes,
                                row,
                                col,
                            });
                        }
                    }
                }
            }
        }
        self.output_offset = self.output_offset.saturating_add(data.len() as u64);
    }

    fn write_terminal(&mut self, bytes: &[u8]) {
        self.term.write(bytes);
        if self.journal.is_some() {
            return;
        }
        self.resize_replay.extend_from_slice(bytes);
        if self.resize_replay.len() > RESIZE_REPLAY_MAX_BYTES {
            // Drop to half the cap in one move: draining exactly to the cap
            // memmoved the whole buffer once per chunk while a session
            // streamed at the cap, which is O(cap) per chunk instead of
            // amortized O(1) per byte.
            let overflow = self.resize_replay.len() - RESIZE_REPLAY_MAX_BYTES / 2;
            self.resize_replay.drain(..overflow);
        }
    }

    /// Enable retaining stripped kitty graphics sequences for passthrough.
    /// Off by default: only a consumer that drains `take_graphics` every
    /// frame (the TUI's live stream) should turn this on.
    pub fn set_graphics_capture(&mut self, enabled: bool) {
        self.apc_filter.capture = enabled;
        if !enabled {
            self.captured_graphics.clear();
        }
    }

    /// Drain captured kitty graphics sequences in arrival order.
    pub fn take_graphics(&mut self) -> Vec<CapturedGraphicsCommand> {
        std::mem::take(&mut self.captured_graphics)
    }

    /// Current cursor position, 0-based (row, col) — the host's terminal
    /// probe responder reports it for CPR (`CSI 6 n`) answers while no
    /// surface is attached.
    pub fn cursor_position(&self) -> (u16, u16) {
        let (col, row) = self.term.cursor();
        (row, col)
    }

    /// True while the fed stream is inside a DEC 2026 synchronized update —
    /// renderers must not snapshot the grid (it is a declared mid-repaint,
    /// typically erased but not yet redrawn).
    pub fn synchronized_output_active(&self) -> bool {
        self.term.synchronized_output()
    }

    /// The DEC private modes an attach client must re-assert after its
    /// replay reset. The reset (RIS) wipes modes the workload negotiated at
    /// startup — usually long before the replayed output tail begins — so a
    /// full-screen mouse app (alt screen + mouse tracking) reattaches with
    /// its content redrawn but its input semantics gone: wheel events scroll
    /// local scrollback instead of reaching the app. The host's parsed VT is
    /// the live truth for these flags.
    pub fn terminal_mode_state(&self) -> TerminalModeState {
        TerminalModeState {
            alt_screen: self.term.alternate_screen(),
            set: RESTORABLE_DEC_MODES_DEFAULT_OFF
                .into_iter()
                .filter(|&mode| self.term.mode(mode))
                .collect(),
            reset: RESTORABLE_DEC_MODES_DEFAULT_ON
                .into_iter()
                .filter(|&mode| !self.term.mode(mode))
                .collect(),
        }
    }

    /// The currently visible screen (the `rows`×`cols` grid, excluding
    /// scrollback) as newline-separated text with trailing blanks trimmed per
    /// row. Mirrors what the phone's `readViewportText()` returns; used to scan
    /// for agent-drawn select-menu footers (see `crate::menu_prompt`).
    pub fn current_screen_text(&mut self) -> String {
        self.term.scroll_bottom();
        let rows = self.term.viewport_rows(false);
        let mut out = String::with_capacity(rows.len() * (self.term.cols as usize + 1));
        for row in rows {
            out.push_str(row.text.trim_end());
            out.push('\n');
        }
        out
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(cols, rows);
    }

    /// The exact lifetime journal offset up to which this VT has consumed
    /// output, paired with a snapshot rendered from that same state. A
    /// client that applies the snapshot and then streams from the returned
    /// offset sees every byte exactly once.
    pub fn snapshot_vt(&self) -> (u64, SnapshotVt) {
        (self.output_offset, render_snapshot_vt(self))
    }

    /// Attach the hosted session's journal. From now on this viewport keeps
    /// no raw output of its own: `snapshot_resized` and scroll offsets past
    /// the resident budget replay `output.bin` instead. Frees the retained
    /// tail immediately.
    pub fn set_journal_session(&mut self, session_id: String) {
        self.journal = Some(JournalSource::Session(session_id));
        self.resize_replay = Vec::new();
    }

    /// Same as `set_journal_session` for a bare journal file.
    pub fn set_journal_path(&mut self, path: PathBuf) {
        self.journal = Some(JournalSource::Path(path));
        self.resize_replay = Vec::new();
    }

    /// Trailing journal bytes (at most `max_bytes`) with the absolute offset
    /// just past them and whether older output exists before the window.
    fn journal_tail(&self, max_bytes: usize) -> Option<(Vec<u8>, u64, bool)> {
        match self.journal.as_ref()? {
            JournalSource::Session(session_id) => {
                let chunk = crate::session_host::read_output_chunk(
                    session_id,
                    None,
                    Some(max_bytes),
                    Some(max_bytes),
                )
                .ok()?;
                let start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                Some((chunk.data, chunk.next_offset, start > 0))
            }
            JournalSource::Path(path) => {
                use std::io::{Read, Seek, SeekFrom};
                let mut file = std::fs::File::open(path).ok()?;
                let len = file.metadata().ok()?.len();
                let mut start = len.saturating_sub(max_bytes as u64);
                file.seek(SeekFrom::Start(start)).ok()?;
                let mut data = Vec::with_capacity((len - start) as usize);
                file.read_to_end(&mut data).ok()?;
                if start > 0 {
                    // Align to a line start so a torn escape sequence at the
                    // window edge cannot corrupt the first replayed row.
                    if let Some(newline) = data.iter().position(|&b| b == b'\n') {
                        data.drain(..=newline);
                        start += newline as u64 + 1;
                    }
                }
                Some((data, len, start > 0))
            }
        }
    }

    /// Replay the journal tail into a scratch terminal and snapshot it.
    fn journal_snapshot(
        &self,
        cols: u16,
        rows: u16,
        max_bytes: usize,
        scrollback_budget: usize,
        scroll_offset_rows: u32,
        viewport_row_count: Option<u16>,
    ) -> Option<TerminalViewportSnapshot> {
        let (data, next_offset, truncated) = self.journal_tail(max_bytes)?;
        let mut scratch = VtTerminal::with_scrollback(cols, rows, scrollback_budget);
        scratch.write(&data);
        Some(snapshot_terminal(
            &mut scratch,
            next_offset,
            truncated,
            scroll_offset_rows,
            viewport_row_count,
        ))
    }

    pub fn snapshot(
        &mut self,
        scroll_offset_rows: u32,
        viewport_row_count: Option<u16>,
    ) -> TerminalViewportSnapshot {
        let live = snapshot_terminal(
            &mut self.term,
            self.output_offset,
            self.history_truncated,
            scroll_offset_rows,
            viewport_row_count,
        );
        // The resident grid holds a bounded scrollback; a request that had
        // to clamp reaches further back through the on-disk journal, which
        // keeps far more history than the grid ever will. Only a strictly
        // deeper replay replaces the live answer, so a journal that lags
        // the grid (batched writes) or is shorter never regresses it.
        if scroll_offset_rows > live.scroll_offset_rows && self.journal.is_some() {
            if let Some(deep) = self.journal_snapshot(
                self.term.cols,
                self.term.rows,
                JOURNAL_SCROLL_REPLAY_MAX_BYTES,
                JOURNAL_SCROLL_REPLAY_MAX_BYTES,
                scroll_offset_rows,
                viewport_row_count,
            ) {
                if deep.scrollback_rows > live.scrollback_rows {
                    return deep;
                }
            }
        }
        live
    }

    /// Non-perturbing snapshot at a different grid size: replays the retained
    /// output tail into a fresh terminal so remote/mobile clients cannot
    /// disturb the live viewport that desktop attach owns via explicit Resize.
    pub fn snapshot_resized(
        &self,
        cols: u16,
        rows: u16,
        scroll_offset_rows: u32,
        viewport_row_count: Option<u16>,
    ) -> TerminalViewportSnapshot {
        if self.journal.is_some() {
            if let Some(snapshot) = self.journal_snapshot(
                cols,
                rows,
                RESIZE_REPLAY_MAX_BYTES,
                MAX_VIEWPORT_SCROLLBACK_BYTES,
                scroll_offset_rows,
                viewport_row_count,
            ) {
                return snapshot;
            }
        }
        let mut replay = VtTerminal::new(cols, rows);
        replay.write(&self.resize_replay);
        snapshot_terminal(
            &mut replay,
            self.output_offset,
            self.history_truncated,
            scroll_offset_rows,
            viewport_row_count,
        )
    }
}

struct TerminalViewportReplayCache {
    cols: u16,
    rows: u16,
    max_bytes: usize,
    output_offset: u64,
    truncated: bool,
    term: VtTerminal,
}

static REPLAY_CACHE: OnceLock<Mutex<HashMap<String, TerminalViewportReplayCache>>> =
    OnceLock::new();

fn replay_cache() -> &'static Mutex<HashMap<String, TerminalViewportReplayCache>> {
    REPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_replay_snapshot(
    session_id: &str,
    cols: u16,
    rows: u16,
    max_bytes: usize,
    output_offset: u64,
    scroll_offset_rows: u32,
    viewport_row_count: Option<u16>,
) -> Option<TerminalViewportSnapshot> {
    let mut guard = replay_cache().lock().ok()?;
    let cache = guard.get_mut(session_id)?;
    if cache.cols == cols
        && cache.rows == rows
        && cache.max_bytes == max_bytes
        && cache.output_offset == output_offset
    {
        let truncated = cache.truncated;
        return Some(snapshot_terminal(
            &mut cache.term,
            output_offset,
            truncated,
            scroll_offset_rows,
            viewport_row_count,
        ));
    }
    None
}

/// Cap on distinct session ids retained in the replay cache. Each entry holds a
/// full virtual terminal (up to `MAX_VIEWPORT_SCROLLBACK_BYTES` of history), so
/// the map is bounded rather than growing once-per-session-id forever. Only ever
/// populated by the one-shot `__viewport__` CLI today, but the bound keeps it
/// safe if a long-lived process ever calls the replay entry points.
const REPLAY_CACHE_MAX_ENTRIES: usize = 64;

fn store_replay_cache(
    session_id: String,
    cols: u16,
    rows: u16,
    max_bytes: usize,
    output_offset: u64,
    truncated: bool,
    term: VtTerminal,
) {
    if let Ok(mut guard) = replay_cache().lock() {
        // Evict when full and this is a new session id. Without access-time
        // tracking a precise LRU isn't worth it here; dropping one arbitrary
        // entry keeps the map bounded (a re-request just re-renders).
        if guard.len() >= REPLAY_CACHE_MAX_ENTRIES && !guard.contains_key(&session_id) {
            if let Some(evict) = guard.keys().next().cloned() {
                guard.remove(&evict);
            }
        }
        guard.insert(
            session_id,
            TerminalViewportReplayCache {
                cols,
                rows,
                max_bytes,
                output_offset,
                truncated,
                term,
            },
        );
    }
}

#[cfg(test)]
pub fn render_terminal_viewport_from_bytes(
    data: &[u8],
    cols: u16,
    rows: u16,
    output_offset: u64,
    truncated: bool,
    scroll_offset_rows: u32,
    viewport_row_count: Option<u16>,
) -> TerminalViewportSnapshot {
    let mut term = VtTerminal::new(cols, rows);
    term.write(data);
    snapshot_terminal(
        &mut term,
        output_offset,
        truncated,
        scroll_offset_rows,
        viewport_row_count,
    )
}

/// Reject session ids that could escape the app-sessions directory. The viewport
/// entry points build `output_path`/`session.sock` paths straight from the id,
/// so an id containing `/`, `..`, `\`, or an absolute path must never be
/// accepted (matches `transcripts::load_safe_manifest` and `mcp_host`).
fn ensure_safe_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains("..")
        || session_id.contains('\\')
    {
        return Err("Invalid session id".into());
    }
    Ok(())
}

pub fn read_terminal_viewport_snapshot(
    session_id: String,
    cols: u16,
    rows: u16,
    max_bytes: Option<usize>,
    scroll_offset_rows: Option<u32>,
    viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    ensure_safe_session_id(&session_id)?;
    if cols == 0 || rows == 0 {
        return Err("Terminal viewport dimensions must be greater than zero".into());
    }

    let scroll_offset_rows = scroll_offset_rows.unwrap_or(0);
    if let Ok(snapshot) = request_terminal_viewport_snapshot(
        &session_id,
        cols,
        rows,
        scroll_offset_rows,
        viewport_rows,
    ) {
        return Ok(snapshot);
    }

    let max_bytes = max_bytes
        .unwrap_or(DEFAULT_VIEWPORT_REPLAY_MAX_BYTES)
        .clamp(1, MAX_VIEWPORT_REPLAY_MAX_BYTES);

    replay_snapshot_from_disk(
        session_id,
        cols,
        rows,
        max_bytes,
        scroll_offset_rows,
        viewport_rows,
    )
}

pub fn replay_terminal_viewport_snapshot(
    session_id: String,
    cols: u16,
    rows: u16,
    max_bytes: Option<usize>,
    scroll_offset_rows: Option<u32>,
    viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    ensure_safe_session_id(&session_id)?;
    if cols == 0 || rows == 0 {
        return Err("Terminal viewport dimensions must be greater than zero".into());
    }

    let scroll_offset_rows = scroll_offset_rows.unwrap_or(0);
    let max_bytes = max_bytes
        .unwrap_or(DEFAULT_VIEWPORT_REPLAY_MAX_BYTES)
        .clamp(1, MAX_VIEWPORT_REPLAY_MAX_BYTES);

    replay_snapshot_from_disk(
        session_id,
        cols,
        rows,
        max_bytes,
        scroll_offset_rows,
        viewport_rows,
    )
}

fn replay_snapshot_from_disk(
    session_id: String,
    cols: u16,
    rows: u16,
    max_bytes: usize,
    scroll_offset_rows: u32,
    viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    if let Ok(metadata) = std::fs::metadata(output_path(&session_id)) {
        if let Some(snapshot) = cached_replay_snapshot(
            &session_id,
            cols,
            rows,
            max_bytes,
            metadata.len(),
            scroll_offset_rows,
            viewport_rows,
        ) {
            return Ok(snapshot);
        }
    }

    let chunk = read_output_chunk(&session_id, None, Some(max_bytes), Some(max_bytes))?;
    let start_offset = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
    let truncated = start_offset > 0;
    let mut term = VtTerminal::new(cols, rows);
    term.write(&chunk.data);
    let snapshot = snapshot_terminal(
        &mut term,
        chunk.next_offset,
        truncated,
        scroll_offset_rows,
        viewport_rows,
    );
    if chunk.exists {
        store_replay_cache(
            session_id,
            cols,
            rows,
            max_bytes,
            chunk.next_offset,
            truncated,
            term,
        );
    }
    Ok(snapshot)
}

pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mode = args.first().map(String::as_str).unwrap_or("snapshot");
    let session_id = args.get(1).ok_or(
        "usage: unpeel-host __viewport__ snapshot <session-id> --cols N --rows N [--max-bytes N] [--scroll-offset-rows N] [--viewport-rows N]",
    )?;
    if mode != "snapshot" {
        return Err(
            "usage: unpeel-host __viewport__ snapshot <session-id> --cols N --rows N [--max-bytes N] [--scroll-offset-rows N] [--viewport-rows N]"
                .to_string(),
        );
    }

    let cols = flag_u16(args, "--cols").unwrap_or(120);
    let rows = flag_u16(args, "--rows").unwrap_or(31);
    let max_bytes = flag_usize(args, "--max-bytes");
    let scroll_offset_rows = flag_u32(args, "--scroll-offset-rows");
    let viewport_rows = flag_u16(args, "--viewport-rows");
    let snapshot = replay_terminal_viewport_snapshot(
        session_id.to_string(),
        cols,
        rows,
        max_bytes,
        scroll_offset_rows,
        viewport_rows,
    )?;
    let body = serde_json::to_string_pretty(&snapshot)
        .map_err(|e| format!("Failed to serialize viewport snapshot: {e}"))?;
    println!("{body}");
    Ok(())
}

fn flag_string(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn flag_usize(args: &[String], name: &str) -> Option<usize> {
    flag_string(args, name)?.parse().ok()
}

fn flag_u32(args: &[String], name: &str) -> Option<u32> {
    flag_string(args, name)?.parse().ok()
}

fn flag_u16(args: &[String], name: &str) -> Option<u16> {
    flag_string(args, name)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        render_terminal_viewport_from_bytes, KittyApcFilter, TerminalViewportState, VtTerminal,
    };

    // Leak-regression: the replay cache is bounded — inserting many distinct
    // session ids evicts old entries instead of growing forever (F3).
    #[test]
    fn replay_cache_is_bounded_by_max_entries() {
        let cap = super::REPLAY_CACHE_MAX_ENTRIES;
        for i in 0..(cap * 3) {
            super::store_replay_cache(
                format!("leak-cache-session-{i}"),
                80,
                24,
                4096,
                i as u64,
                false,
                VtTerminal::new(80, 24),
            );
        }
        let len = super::replay_cache().lock().unwrap().len();
        assert!(len <= cap, "replay cache grew to {len}, cap is {cap}");
    }

    #[test]
    fn viewport_snapshot_rejects_traversal_session_ids() {
        for bad in ["../../etc", "a/b", "..", "", "x\\y"] {
            assert!(
                super::read_terminal_viewport_snapshot(bad.to_string(), 80, 24, None, None, None)
                    .is_err(),
                "expected {bad:?} to be rejected"
            );
            assert!(super::replay_terminal_viewport_snapshot(
                bad.to_string(),
                80,
                24,
                None,
                None,
                None
            )
            .is_err());
        }
    }

    fn rows(snapshot: &super::TerminalViewportSnapshot) -> Vec<String> {
        snapshot
            .viewport_rows
            .iter()
            .map(|row| row.text.clone())
            .collect()
    }

    fn numbered_lines(count: usize) -> Vec<u8> {
        let mut output = String::new();
        for index in 0..count {
            if index > 0 {
                output.push_str("\r\n");
            }
            output.push_str(&format!("line{index:03}"));
        }
        output.into_bytes()
    }

    #[test]
    fn viewport_renders_basic_text_and_scrolls() {
        let snapshot =
            render_terminal_viewport_from_bytes(b"one\r\ntwo\r\nthree", 8, 2, 15, false, 0, None);
        assert_eq!(rows(&snapshot), vec!["two", "three"]);
        assert_eq!(snapshot.scrollback_rows, 1);
        let history =
            render_terminal_viewport_from_bytes(b"one\r\ntwo\r\nthree", 8, 2, 15, false, 1, None);
        assert_eq!(rows(&history), vec!["one", "two"]);
        assert_eq!(snapshot.cursor_row, 1);
        assert_eq!(snapshot.cursor_col, 5);
    }

    #[test]
    fn viewport_returns_requested_history_window() {
        let output = numbered_lines(10);
        let snapshot = render_terminal_viewport_from_bytes(
            &output,
            8,
            3,
            output.len() as u64,
            false,
            2,
            Some(5),
        );

        assert_eq!(snapshot.scrollback_rows, 7);
        assert_eq!(snapshot.scroll_offset_rows, 2);
        assert_eq!(snapshot.viewport_start_row, 3);
        assert_eq!(
            rows(&snapshot),
            vec!["line003", "line004", "line005", "line006", "line007"]
        );
    }

    #[test]
    fn viewport_clamps_scroll_offset_to_available_history() {
        let output = numbered_lines(4);
        let snapshot = render_terminal_viewport_from_bytes(
            &output,
            8,
            2,
            output.len() as u64,
            false,
            999,
            None,
        );

        assert_eq!(snapshot.scroll_offset_rows, 2);
        assert_eq!(snapshot.viewport_start_row, 0);
        assert_eq!(rows(&snapshot), vec!["line000", "line001"]);
    }

    #[test]
    fn viewport_clamps_requested_window_to_available_rows() {
        let output = numbered_lines(3);
        let snapshot = render_terminal_viewport_from_bytes(
            &output,
            8,
            2,
            output.len() as u64,
            false,
            0,
            Some(99),
        );

        assert_eq!(snapshot.viewport_rows.len(), 3);
        assert_eq!(snapshot.viewport_start_row, 0);
        assert_eq!(rows(&snapshot), vec!["line000", "line001", "line002"]);
    }

    #[test]
    fn viewport_handles_cursor_position_and_erase_line() {
        let snapshot = render_terminal_viewport_from_bytes(
            b"abcdef\x1b[1;3HXY\x1b[K",
            8,
            2,
            14,
            false,
            0,
            None,
        );
        assert_eq!(rows(&snapshot), vec!["abXY", ""]);
        assert_eq!(snapshot.cursor_row, 0);
        assert_eq!(snapshot.cursor_col, 4);
    }

    #[test]
    fn viewport_tracks_sgr_styles() {
        let snapshot =
            render_terminal_viewport_from_bytes(b"\x1b[1;31mR\x1b[0mN", 4, 1, 12, false, 0, None);
        assert_eq!(rows(&snapshot), vec!["RN"]);
        assert_eq!(snapshot.viewport_rows[0].styles.len(), 1);
        let style = &snapshot.viewport_rows[0].styles[0];
        assert_eq!(style.start, 0);
        assert_eq!(style.len, 1);
        assert_eq!(style.fg.as_deref(), Some("ansi:1"));
        assert!(style.bold);
    }

    #[test]
    fn viewport_merges_adjacent_style_cells_and_omits_default_gaps() {
        let snapshot = render_terminal_viewport_from_bytes(
            b"\x1b[31mAB\x1b[0mC\x1b[31mD",
            6,
            1,
            18,
            false,
            0,
            None,
        );

        assert_eq!(rows(&snapshot), vec!["ABCD"]);
        assert_eq!(snapshot.viewport_rows[0].styles.len(), 2);
        assert_eq!(snapshot.viewport_rows[0].styles[0].start, 0);
        assert_eq!(snapshot.viewport_rows[0].styles[0].len, 2);
        assert_eq!(
            snapshot.viewport_rows[0].styles[0].fg.as_deref(),
            Some("ansi:1")
        );
        assert_eq!(snapshot.viewport_rows[0].styles[1].start, 3);
        assert_eq!(snapshot.viewport_rows[0].styles[1].len, 1);
    }

    #[test]
    fn viewport_style_runs_cover_inverse_spaces() {
        let snapshot =
            render_terminal_viewport_from_bytes(b"\x1b[7m A \x1b[0mZ", 5, 1, 13, false, 0, None);

        assert_eq!(rows(&snapshot), vec![" A Z"]);
        assert_eq!(snapshot.viewport_rows[0].styles.len(), 1);
        assert_eq!(snapshot.viewport_rows[0].styles[0].start, 0);
        assert_eq!(snapshot.viewport_rows[0].styles[0].len, 3);
        assert!(snapshot.viewport_rows[0].styles[0].inverse);
    }

    #[test]
    fn viewport_style_runs_cover_erased_background_to_the_edge() {
        // Codex paints each composer row by setting a background and using
        // EL. Ghostty stores those erased cells as background-only content
        // tags rather than ordinary styled cells.
        let snapshot = render_terminal_viewport_from_bytes(
            b"\x1b[48;2;53;53;57m\x1b[K",
            8,
            1,
            21,
            false,
            0,
            None,
        );

        assert_eq!(snapshot.viewport_rows[0].text, "");
        assert_eq!(snapshot.viewport_rows[0].styles.len(), 1);
        let style = &snapshot.viewport_rows[0].styles[0];
        assert_eq!(style.start, 0);
        assert_eq!(style.len, 8);
        assert_eq!(style.bg.as_deref(), Some("rgb:53,53,57"));
    }

    #[test]
    fn viewport_trims_default_trailing_spaces_from_payload() {
        let snapshot = render_terminal_viewport_from_bytes(b"x", 80, 2, 1, false, 0, None);

        assert_eq!(snapshot.viewport_rows.len(), 2);
        assert_eq!(snapshot.viewport_rows[0].text, "x");
        assert_eq!(snapshot.viewport_rows[1].text, "");
    }

    #[test]
    fn viewport_tracks_extended_sgr_colors() {
        let snapshot = render_terminal_viewport_from_bytes(
            b"\x1b[38;5;196mA\x1b[48;2;1;2;3mB",
            4,
            1,
            28,
            false,
            0,
            None,
        );

        assert_eq!(rows(&snapshot), vec!["AB"]);
        assert_eq!(snapshot.viewport_rows[0].styles.len(), 2);
        assert_eq!(
            snapshot.viewport_rows[0].styles[0].fg.as_deref(),
            Some("ansi256:196")
        );
        assert_eq!(
            snapshot.viewport_rows[0].styles[1].bg.as_deref(),
            Some("rgb:1,2,3")
        );
    }

    #[test]
    fn viewport_handles_save_restore_cursor() {
        let snapshot =
            render_terminal_viewport_from_bytes(b"ab\x1b7cd\x1b8Z", 6, 1, 9, false, 0, None);

        assert_eq!(rows(&snapshot), vec!["abZd"]);
        assert_eq!(snapshot.cursor_row, 0);
        assert_eq!(snapshot.cursor_col, 3);
    }

    #[test]
    fn viewport_ignores_osc_payloads() {
        let snapshot =
            render_terminal_viewport_from_bytes(b"a\x1b]0;title\x07b", 4, 1, 12, false, 0, None);
        assert_eq!(rows(&snapshot), vec!["ab"]);
    }

    #[test]
    fn viewport_state_handles_split_csi_sequences() {
        let mut state = TerminalViewportState::new(4, 1);
        state.feed(b"\x1b[31");
        state.feed(b"mR\x1b[0");
        state.feed(b"mN");
        let snapshot = state.snapshot(0, None);

        assert_eq!(rows(&snapshot), vec!["RN"]);
        assert_eq!(snapshot.viewport_rows[0].styles.len(), 1);
        assert_eq!(
            snapshot.viewport_rows[0].styles[0].fg.as_deref(),
            Some("ansi:1")
        );
    }

    #[test]
    fn viewport_state_handles_split_osc_sequences() {
        let mut state = TerminalViewportState::new(6, 1);
        state.feed(b"a\x1b]0;ti");
        state.feed(b"tle\x07b");
        let snapshot = state.snapshot(0, None);

        assert_eq!(rows(&snapshot), vec!["ab"]);
    }

    #[test]
    fn viewport_state_tracks_synchronized_output_mode() {
        let mut state = TerminalViewportState::new(8, 2);
        assert!(!state.synchronized_output_active());
        state.feed(b"\x1b[?2026h\x1b[2Jrepainting");
        assert!(state.synchronized_output_active());
        // Split across a chunk boundary, like the socket delivers it.
        state.feed(b"\x1b[?20");
        assert!(state.synchronized_output_active());
        state.feed(b"26l");
        assert!(!state.synchronized_output_active());
    }

    #[test]
    fn viewport_snapshot_reports_when_the_child_owns_mouse_buttons() {
        let off = render_terminal_viewport_from_bytes(b"plain text", 8, 2, 10, false, 0, None);
        assert!(!off.mouse_reporting);

        let on = render_terminal_viewport_from_bytes(
            b"\x1b[?1002h\x1b[?1006hclickable",
            8,
            2,
            20,
            false,
            0,
            None,
        );
        assert!(on.input_modes_known);
        assert!(on.mouse_reporting);
        assert!(on.mouse_button_motion);
        assert!(!on.mouse_any_motion);
        assert!(!on.alternate_screen);

        let reset = render_terminal_viewport_from_bytes(
            b"\x1b[?1003h\x1b[?1003lplain",
            8,
            2,
            20,
            false,
            0,
            None,
        );
        assert!(!reset.mouse_reporting);

        let alternate = render_terminal_viewport_from_bytes(
            b"\x1b[?1049h\x1b[?1007h\x1b[?1hfull screen",
            8,
            2,
            30,
            false,
            0,
            None,
        );
        assert!(alternate.alternate_screen);
        assert!(alternate.mouse_alternate_scroll);
        assert!(alternate.application_cursor);
    }

    #[test]
    fn viewport_rows_preserve_soft_wrap_for_clipboard_unwrapping() {
        let snapshot = render_terminal_viewport_from_bytes(b"abcdefgh", 4, 2, 8, false, 0, None);
        assert_eq!(snapshot.viewport_rows[0].text, "abcd");
        assert!(snapshot.viewport_rows[0].wrapped);
        assert_eq!(snapshot.viewport_rows[1].text, "efgh");
        assert!(!snapshot.viewport_rows[1].wrapped);
    }

    #[test]
    fn viewport_state_incremental_feed_matches_batch_parse() {
        let chunks = [
            b"one\r\n".as_slice(),
            b"\x1b[31".as_slice(),
            b"mtwo\x1b[0m\r\nthr".as_slice(),
            b"ee".as_slice(),
        ];
        let mut state = TerminalViewportState::new(8, 2);
        for chunk in chunks {
            state.feed(chunk);
        }
        let incremental = state.snapshot(0, None);
        let batch = render_terminal_viewport_from_bytes(
            b"one\r\n\x1b[31mtwo\x1b[0m\r\nthree",
            8,
            2,
            24,
            false,
            0,
            None,
        );

        assert_eq!(rows(&incremental), rows(&batch));
        assert_eq!(
            incremental.viewport_rows[0].styles,
            batch.viewport_rows[0].styles
        );
    }

    #[test]
    fn viewport_state_tracks_absolute_output_offset() {
        let mut state = TerminalViewportState::new(8, 2);
        assert_eq!(state.output_offset(), 0);

        state.feed(b"one");
        state.feed(b" two");

        assert_eq!(state.output_offset(), 7);
        assert_eq!(state.snapshot(0, None).output_offset, 7);
    }

    #[test]
    fn viewport_state_reset_rebases_and_reports_truncated_history() {
        let mut state = TerminalViewportState::new(8, 2);
        state.feed(b"stale output");

        state.reset_at_output_offset(6, 1, 40, true);
        assert_eq!(state.output_offset(), 40);
        state.feed(b"fresh");

        let snapshot = state.snapshot(0, None);
        assert_eq!(snapshot.cols, 6);
        assert_eq!(snapshot.rows, 1);
        assert_eq!(snapshot.output_offset, 45);
        assert!(snapshot.truncated);
        assert_eq!(rows(&snapshot), vec!["fresh"]);

        let resized = state.snapshot_resized(10, 2, 0, None);
        assert_eq!(resized.output_offset, 45);
        assert!(resized.truncated);
        assert_eq!(rows(&resized), vec!["fresh", ""]);
    }

    #[test]
    fn viewport_state_resize_preserves_bottom_rows() {
        let mut state = TerminalViewportState::new(8, 2);
        state.feed(&numbered_lines(4));
        state.resize(8, 3);
        let snapshot = state.snapshot(0, None);

        assert_eq!(rows(&snapshot), vec!["line001", "line002", "line003"]);
        assert_eq!(snapshot.scrollback_rows, 1);
    }

    #[test]
    fn viewport_state_resize_smaller_moves_trimmed_rows_to_scrollback() {
        let mut state = TerminalViewportState::new(8, 4);
        state.feed(&numbered_lines(4));
        state.resize(8, 2);

        let bottom = state.snapshot(0, None);
        assert_eq!(rows(&bottom), vec!["line002", "line003"]);
        assert_eq!(bottom.scrollback_rows, 2);

        let history = state.snapshot(2, None);
        assert_eq!(rows(&history), vec!["line000", "line001"]);
    }

    #[test]
    fn viewport_state_snapshot_resized_does_not_mutate_state() {
        let mut state = TerminalViewportState::new(8, 4);
        state.feed(&numbered_lines(4));

        let smaller = state.snapshot_resized(8, 2, 0, None);
        assert_eq!(rows(&smaller), vec!["line002", "line003"]);

        let original = state.snapshot(0, None);
        assert_eq!(
            rows(&original),
            vec!["line000", "line001", "line002", "line003"]
        );
        assert_eq!(original.rows, 4);
    }

    #[test]
    fn kitty_graphics_payloads_are_stripped_from_the_viewport_feed() {
        let mut state = TerminalViewportState::new(20, 4);
        // Text, then a kitty image split across chunk boundaries (including
        // a split introducer), then more text — the screen must contain the
        // text only, and mode sequences around the image must still land.
        state.feed(b"before\x1b");
        state.feed(b"_Ga=T,f=100;QUJDRA==\x1b");
        state.feed(b"\\\x1b[?1002h after");
        let screen = state.current_screen_text();
        assert!(screen.contains("before after"), "screen: {screen:?}");
        assert!(!screen.contains('Q'), "image payload leaked: {screen:?}");
        assert_eq!(state.terminal_mode_state().set, vec![1002]);
    }

    #[test]
    fn graphics_capture_retains_sequences_with_cursor_anchors() {
        let mut state = TerminalViewportState::new(40, 10);
        state.set_graphics_capture(true);
        // A Surface-style present: home the cursor, then place; and a second
        // image after moving to row 3 col 5 (1-based CSI -> 0-based anchor).
        state.feed(b"\x1b[H\x1b_Ga=T,f=32,s=8,v=8,i=7;AAAA\x1b\\");
        state.feed(b"\x1b[3;5H\x1b_Ga=T,i=9;BBBB\x1b");
        state.feed(b"\\tail");
        let captured = state.take_graphics();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].bytes, b"\x1b_Ga=T,f=32,s=8,v=8,i=7;AAAA\x1b\\");
        assert_eq!((captured[0].row, captured[0].col), (0, 0));
        assert_eq!(captured[1].bytes, b"\x1b_Ga=T,i=9;BBBB\x1b\\");
        assert_eq!((captured[1].row, captured[1].col), (2, 4));
        // Drained: a second take is empty; the screen never saw the payload.
        assert!(state.take_graphics().is_empty());
        assert!(state.current_screen_text().contains("tail"));
        assert!(!state.current_screen_text().contains("AAAA"));
    }

    #[test]
    fn graphics_capture_is_off_by_default_and_bounded() {
        let mut state = TerminalViewportState::new(20, 4);
        state.feed(b"\x1b_Ga=T,i=1;AAAA\x1b\\");
        assert!(state.take_graphics().is_empty());

        state.set_graphics_capture(true);
        for index in 0..(super::MAX_CAPTURED_GRAPHICS + 10) {
            state.feed(format!("\x1b_Ga=T,i={index};AA\x1b\\").as_bytes());
        }
        let captured = state.take_graphics();
        assert_eq!(captured.len(), super::MAX_CAPTURED_GRAPHICS);
        // Oldest dropped first: the newest sequence is retained.
        let last = String::from_utf8_lossy(&captured.last().unwrap().bytes).into_owned();
        assert!(last.contains(&format!("i={}", super::MAX_CAPTURED_GRAPHICS + 9)));
    }

    #[test]
    fn non_kitty_apc_and_plain_escapes_pass_through_the_filter() {
        let mut filter = KittyApcFilter::default();
        // Plain escape split at the chunk boundary: withheld ESC re-emitted.
        assert_eq!(filter.filter(b"ok\x1b"), Some(b"ok".to_vec()));
        assert_eq!(filter.filter(b"[2J"), Some(b"\x1b[2J".to_vec()));
        // Non-kitty APC (tmux passthrough shape) is not stripped.
        assert_eq!(
            filter.filter(b"\x1b_tmux;x\x1b\\"),
            Some(b"\x1b_tmux;x\x1b\\".to_vec())
        );
        // Pure text takes the zero-copy fast path.
        assert_eq!(filter.filter(b"plain text"), None);
    }

    #[test]
    fn terminal_mode_state_tracks_mouse_alt_screen_and_restores_them() {
        let mut state = TerminalViewportState::new(20, 4);
        assert!(state.terminal_mode_state().is_default());

        // A full-screen mouse app's startup: alt screen, SGR mouse tracking,
        // bracketed paste, cursor hidden.
        state.feed(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?25l");
        let modes = state.terminal_mode_state();
        assert!(modes.alt_screen);
        assert_eq!(modes.set, vec![1002, 1006, 2004]);
        assert_eq!(modes.reset, vec![25]);
        assert_eq!(
            modes.restore_sequence(),
            b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?25l".to_vec()
        );

        // App exit restores everything; the manifest field goes away.
        state.feed(b"\x1b[?25h\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1049l");
        assert!(state.terminal_mode_state().is_default());
        assert!(state.terminal_mode_state().restore_sequence().is_empty());
    }

    #[test]
    fn current_screen_text_feeds_the_menu_detector() {
        use crate::menu_prompt::viewport_has_menu_prompt;

        let mut state = TerminalViewportState::new(60, 6);
        state.feed(b"\x1b[2J\x1b[H"); // clear + home
        state.feed(b"  1. Switch to $59/year\r\n");
        state.feed(b"  2. Keep perpetual one-time\r\n");
        state.feed(b"\r\n");
        state.feed(b"Enter to select \xc2\xb7 \xe2\x86\x91/\xe2\x86\x93 to navigate \xc2\xb7 Esc to cancel");

        let screen = state.current_screen_text();
        assert!(screen.contains("1. Switch to $59/year"));
        assert!(viewport_has_menu_prompt(&screen));

        // Answering the menu redraws the screen; the footer is gone, so the
        // detector must clear.
        state.feed(b"\x1b[2J\x1b[HWorking on it\xe2\x80\xa6");
        assert!(!viewport_has_menu_prompt(&state.current_screen_text()));
    }

    #[test]
    fn viewport_retains_bounded_scrollback() {
        // Feed well past the byte budget twice; scrollback must trim to a
        // steady state instead of growing with input (ghostty trims with
        // page granularity, so exact row counts are not asserted).
        let mut state = TerminalViewportState::new(80, 4);
        let block = format!("{}\r\n", "x".repeat(78)).repeat(100_000);

        state.feed(block.as_bytes());
        let first = state.snapshot(0, Some(1)).scrollback_rows;
        assert!(
            (first as usize) < 100_000,
            "scrollback {first} was never trimmed"
        );

        state.feed(block.as_bytes());
        let second = state.snapshot(0, Some(1)).scrollback_rows;
        assert!(
            second <= first.saturating_add(first / 4),
            "scrollback kept growing: {first} -> {second}"
        );

        // The 256 KiB budget keeps a useful window of history at 80 cols
        // without ballooning: bounded above by the byte budget, and still at
        // least a few screens deep.
        assert!(
            (second as usize) * 80 <= super::MAX_VIEWPORT_SCROLLBACK_BYTES * 2,
            "scrollback {second} rows exceeds the byte budget"
        );
        assert!(
            second >= 100,
            "scrollback trimmed too aggressively: {second}"
        );

        // A scroll offset past the retained rows clamps to what is left
        // instead of failing: readers that remembered a deeper offset from
        // before trimming (or a larger budget) still get the oldest rows.
        let deep = state.snapshot(u32::MAX, Some(2));
        assert_eq!(deep.viewport_start_row, 0);
        // total rows = scrollback + 4 grid rows, window of 2 → offset clamps to
        // scrollback + 2.
        assert_eq!(deep.scroll_offset_rows, second + 2);
        assert_eq!(deep.viewport_rows.len(), 2);
        assert!(deep.viewport_rows[0].text.starts_with("xxxx"));
    }

    #[test]
    fn viewport_handles_wide_characters() {
        // "日本" is two wide chars (4 columns); the spacer cells must not
        // inject extra columns into the text payload.
        let snapshot =
            render_terminal_viewport_from_bytes("日本x".as_bytes(), 8, 1, 7, false, 0, None);
        assert_eq!(rows(&snapshot), vec!["日本x"]);
    }

    #[test]
    fn scroll_past_resident_budget_reads_the_journal() {
        // The resident grid keeps a bounded scrollback; the same bytes live
        // in the journal. A scroll offset the grid had to clamp must come
        // back from the journal with real rows, never an error or blanks.
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("output.bin");
        let block = (0..100_000)
            .map(|i| format!("{i:06} {}\r\n", "x".repeat(70)))
            .collect::<String>()
            .into_bytes();
        std::fs::write(&journal, &block).unwrap();

        let mut state = TerminalViewportState::new(80, 4);
        state.set_journal_path(journal);
        state.feed(&block);

        let resident = state.snapshot(0, Some(1)).scrollback_rows;
        assert!(
            (resident as usize) < 100_000,
            "resident scrollback never trimmed"
        );

        let deep = state.snapshot(u32::MAX, Some(2));
        assert!(
            deep.scrollback_rows > resident,
            "journal replay ({}) was not deeper than the resident grid ({resident})",
            deep.scrollback_rows
        );
        assert_eq!(deep.viewport_start_row, 0);
        assert_eq!(deep.viewport_rows.len(), 2);
        assert!(
            deep.viewport_rows[0].text.contains("xxxx"),
            "journal-backed row is blank: {:?}",
            deep.viewport_rows[0].text
        );
        assert!(deep.truncated, "a 7.8 MB journal exceeds the replay window");

        // Within the resident range the live grid still answers directly.
        let shallow = state.snapshot(10, Some(1));
        assert_eq!(shallow.scroll_offset_rows, 10);
        assert_eq!(shallow.scrollback_rows, resident);
    }

    /// Resident memory of this process in KiB: macOS phys_footprint (the
    /// metric the comparison charts use), Linux PSS from smaps_rollup.
    fn process_resident_kib() -> u64 {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("footprint")
                .arg(std::process::id().to_string())
                .output()
                .expect("footprint(1)");
            let text = String::from_utf8_lossy(&out.stdout);
            let line = text
                .lines()
                .find(|l| l.trim_start().starts_with("phys_footprint:"))
                .expect("phys_footprint line");
            let mut parts = line.split_whitespace().skip(1);
            let value: f64 = parts.next().unwrap().parse().unwrap();
            let unit = parts.next().unwrap_or("KB");
            match unit {
                "MB" => (value * 1024.0) as u64,
                "GB" => (value * 1024.0 * 1024.0) as u64,
                _ => value as u64,
            }
        }
        #[cfg(target_os = "linux")]
        {
            let text = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
            text.lines()
                .find(|l| l.starts_with("Pss:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
                .unwrap()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            0
        }
    }

    /// First hard data on what libghostty-vt commits per terminal: build 1
    /// then 50 viewports at 80x24 and report the resident delta per
    /// instance, empty and after 10k lines of 72-col prose, for several
    /// scrollback budgets, with the journal attached (no raw-output copy)
    /// and without. Ignored in the normal suite; run it in release:
    ///
    ///   cargo test --release -p unpeel-core --lib vt_footprint -- --ignored --nocapture
    #[test]
    #[ignore]
    fn vt_footprint_per_terminal() {
        let filled: Vec<u8> = (0..10_000)
            .map(|i| format!("{i:05} {}\r\n", "x".repeat(66)))
            .collect::<String>()
            .into_bytes();
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("output.bin");
        std::fs::write(&journal, &filled).unwrap();
        let settle = || std::thread::sleep(std::time::Duration::from_millis(50));
        // Warm the allocator and the VT library once so the first-instance
        // delta is not dominated by one-time setup.
        {
            let mut warm = TerminalViewportState::new(80, 24);
            warm.feed(&filled);
            let _ = warm.snapshot(0, None);
        }

        let make = |budget: usize, with_journal: bool| {
            let mut state = TerminalViewportState::new(80, 24);
            state.term = VtTerminal::with_scrollback(80, 24, budget);
            if with_journal {
                state.set_journal_path(journal.clone());
            }
            state
        };
        for (budget, with_journal) in [
            (super::MAX_VIEWPORT_SCROLLBACK_BYTES, false),
            (super::MAX_VIEWPORT_SCROLLBACK_BYTES, true),
            (1, true),
            (16 * 1024, true),
            (64 * 1024, true),
            (128 * 1024, true),
            (512 * 1024, true),
            (1024 * 1024, true),
            (2 * 1024 * 1024, true),
        ] {
            super::release_memory_to_os();
            settle();
            let base = process_resident_kib();
            let one = make(budget, with_journal);
            settle();
            let after_one = process_resident_kib();
            let mut many: Vec<TerminalViewportState> =
                (0..50).map(|_| make(budget, with_journal)).collect();
            settle();
            let after_fifty_empty = process_resident_kib();
            let feed_started = std::time::Instant::now();
            for state in &mut many {
                state.feed(&filled);
            }
            let feed_mib_s = (filled.len() as f64 * 50.0 / 1048576.0)
                / feed_started.elapsed().as_secs_f64().max(1e-9);
            settle();
            let after_fifty_filled = process_resident_kib();
            let deep = many[0].snapshot(u32::MAX, Some(1));
            drop(many);
            drop(one);
            settle();
            let dropped = process_resident_kib();
            super::release_memory_to_os();
            settle();
            let released = process_resident_kib();
            let per_empty = after_fifty_empty.saturating_sub(after_one) / 50;
            let per_filled = after_fifty_filled.saturating_sub(after_fifty_empty) / 50;
            println!(
                "budget {:>4} KiB journal={:<5} | empty +{:>4} KiB | per empty +{:>4} KiB | per filled +{:>5} KiB | feed {:>6.1} MiB/s | resident rows {:>5} | 50 filled {:>7} KiB | dropped {:>+6} KiB | released {:>+6} KiB",
                budget / 1024,
                with_journal,
                after_one.saturating_sub(base),
                per_empty,
                per_filled,
                feed_mib_s,
                deep.scrollback_rows,
                after_fifty_filled,
                dropped as i64 - base as i64,
                released as i64 - base as i64,
            );
            // The Host configuration (default budget, journal attached) is
            // the chart row; scripts/bench-memory.sh parses this line.
            if budget == super::MAX_VIEWPORT_SCROLLBACK_BYTES && with_journal {
                println!(
                    "VT_ROW empty_kib={per_empty} filled_kib={per_filled} feed_mib_s={feed_mib_s:.1}"
                );
            }
        }
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("vmmap")
                .args(["-summary", &std::process::id().to_string()])
                .output()
                .expect("vmmap");
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text
                .lines()
                .filter(|l| l.starts_with("MALLOC") || l.starts_with("TOTAL"))
            {
                println!("  vmmap: {line}");
            }
        }
    }
}

#[cfg(test)]
mod snapshot_vt_tests {
    //! Round trips: feed bytes into VT A, render its snapshot, feed the
    //! snapshot into a fresh VT B of the same size, and compare the two
    //! terminals cell by cell (text, full style, resolved background),
    //! plus cursor, active screen, scrollback depth, and every input mode
    //! the attach client cares about.
    use super::*;

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct CellFingerprint {
        text: String,
        style: String,
        bg: (u8, u8, u8),
    }

    fn color_key(color: &vt::GhosttyStyleColor) -> String {
        match color.tag {
            vt::GHOSTTY_STYLE_COLOR_PALETTE => format!("p{}", unsafe { color.value.palette }),
            vt::GHOSTTY_STYLE_COLOR_RGB => {
                let rgb = unsafe { color.value.rgb };
                format!("rgb{},{},{}", rgb.r, rgb.g, rgb.b)
            }
            _ => "-".into(),
        }
    }

    fn style_key(style: &vt::GhosttyStyle) -> String {
        format!(
            "fg={} bg={} ul={} b={} i={} f={} bl={} inv={} inv2={} s={} o={} u={}",
            color_key(&style.fg_color),
            color_key(&style.bg_color),
            color_key(&style.underline_color),
            style.bold,
            style.italic,
            style.faint,
            style.blink,
            style.inverse,
            style.invisible,
            style.strikethrough,
            style.overline,
            style.underline
        )
    }

    /// Every visible cell of the current viewport with its full style.
    fn grid(term: &mut VtTerminal) -> Vec<Vec<CellFingerprint>> {
        term.scroll_bottom();
        let mut rows = Vec::new();
        with_render_scratch(term, |scratch| unsafe {
            assert_eq!(
                vt::ghostty_render_state_get(
                    scratch.render,
                    vt::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                    &mut scratch.row_iter as *mut _ as *mut _,
                ),
                vt::GHOSTTY_SUCCESS
            );
            while vt::ghostty_render_state_row_iterator_next(scratch.row_iter) {
                let mut row = Vec::new();
                if vt::ghostty_render_state_row_get(
                    scratch.row_iter,
                    vt::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                    &mut scratch.cells as *mut _ as *mut _,
                ) != vt::GHOSTTY_SUCCESS
                {
                    rows.push(row);
                    continue;
                }
                let mut previous = CellStyle::default();
                while vt::ghostty_render_state_row_cells_next(scratch.cells) {
                    let (text, _) = read_cell(scratch.cells, true, &previous);
                    previous = CellStyle::default();
                    let mut style = vt::GhosttyStyle::zeroed_sized();
                    let mut has_styling = false;
                    let _ = vt::ghostty_render_state_row_cells_get(
                        scratch.cells,
                        vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
                        &mut has_styling as *mut _ as *mut _,
                    );
                    let style = if has_styling
                        && vt::ghostty_render_state_row_cells_get(
                            scratch.cells,
                            vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                            &mut style as *mut _ as *mut _,
                        ) == vt::GHOSTTY_SUCCESS
                    {
                        style_key(&style)
                    } else {
                        "default".into()
                    };
                    let mut rgb = vt::GhosttyColorRgb::default();
                    let _ = vt::ghostty_render_state_row_cells_get(
                        scratch.cells,
                        vt::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                        &mut rgb as *mut _ as *mut _,
                    );
                    row.push(CellFingerprint {
                        text,
                        style,
                        bg: (rgb.r, rgb.g, rgb.b),
                    });
                }
                rows.push(row);
            }
        });
        rows
    }

    const MODES: [u16; 14] = [
        1, 6, 7, 9, 25, 1000, 1002, 1003, 1004, 1006, 1049, 2004, 2026, 2027,
    ];

    #[derive(Debug, PartialEq, Eq)]
    struct StateFingerprint {
        cursor: (u16, u16),
        cursor_visible: bool,
        alternate: bool,
        scrollback_rows: usize,
        modes: Vec<(u16, bool)>,
    }

    fn state(term: &VtTerminal) -> StateFingerprint {
        StateFingerprint {
            cursor: term.cursor(),
            cursor_visible: term.mode(25),
            alternate: term.alternate_screen(),
            scrollback_rows: term.scrollback_rows(),
            modes: MODES.iter().map(|&mode| (mode, term.mode(mode))).collect(),
        }
    }

    /// Feed `input` into a fresh VT, snapshot it, replay the snapshot into a
    /// second fresh VT, and require both terminals to be indistinguishable.
    /// Returns the snapshot bytes for additional assertions.
    fn round_trip(cols: u16, rows: u16, input: &[u8]) -> Vec<u8> {
        let mut a = VtTerminal::new(cols, rows);
        a.write(input);
        let bytes = a.render_snapshot_vt();
        let mut b = VtTerminal::new(cols, rows);
        b.write(&bytes);
        let grid_a = grid(&mut a);
        let grid_b = grid(&mut b);
        for (y, (ra, rb)) in grid_a.iter().zip(grid_b.iter()).enumerate() {
            assert_eq!(
                ra,
                rb,
                "row {y} differs\nsnapshot: {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
        assert_eq!(grid_a.len(), grid_b.len());
        assert_eq!(
            state(&a),
            state(&b),
            "state differs\nsnapshot: {:?}",
            String::from_utf8_lossy(&bytes)
        );
        bytes
    }

    #[test]
    fn empty_terminal_round_trips_to_empty_snapshot_with_home_cursor() {
        let bytes = round_trip(80, 24, b"");
        assert!(
            bytes.ends_with(b"\x1b[1;1H"),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(
            !bytes
                .iter()
                .any(|b| b.is_ascii_alphabetic() && !b"gGHm".contains(b)),
            "no content on an empty screen: {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn plain_text_and_cursor_round_trip() {
        round_trip(40, 10, b"hello\r\nworld\r\n$ ");
    }

    #[test]
    fn sixteen_256_and_truecolor_styles_round_trip() {
        round_trip(
            60,
            8,
            b"\x1b[31mred\x1b[0m \x1b[1;44mbold-on-blue\x1b[0m \x1b[38;5;208mo208\x1b[0m \
              \x1b[48;5;17mbg17\x1b[0m \x1b[38;2;10;200;30mtrue\x1b[0m \x1b[48;2;250;20;120mtbg\x1b[0m\r\n\
              \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m \x1b[4:3mcurly\x1b[0m \
              \x1b[58;2;1;2;3;4mulcolor\x1b[0m \x1b[7minverse\x1b[0m \x1b[9mstrike\x1b[0m \
              \x1b[5mblink\x1b[0m \x1b[53moverline\x1b[0m \x1b[8mhidden\x1b[0m\r\n\
              \x1b[1;3;4;31;42mall at once",
        );
    }

    #[test]
    fn wide_characters_and_grapheme_clusters_round_trip() {
        round_trip(
            30,
            6,
            "日本語 テキスト\r\n👩‍👩‍👧‍👦 family 🇳🇴 flag e\u{301} combining\r\n\x1b[32m漢\x1b[0m字"
                .as_bytes(),
        );
    }

    #[test]
    fn wide_character_split_at_the_margin_round_trips() {
        // 9 narrow cells then a wide char that cannot fit in the last
        // column: a spacer head is left and the char wraps.
        round_trip(10, 4, "abcdefghi日本".as_bytes());
    }

    #[test]
    fn scrollback_rows_are_included_and_the_screen_matches() {
        let mut input = Vec::new();
        for i in 0..60 {
            input.extend_from_slice(
                format!("line {i:03} \x1b[3{}mcolor\x1b[0m\r\n", i % 8).as_bytes(),
            );
        }
        input.extend_from_slice(b"prompt> ");
        let bytes = round_trip(40, 24, &input);
        assert!(
            bytes.windows(8).any(|w| w == b"line 000"),
            "scrollback carried"
        );
    }

    #[test]
    fn alternate_screen_full_frame_round_trips() {
        let mut frame = Vec::new();
        frame.extend_from_slice(b"\x1b[?1049h\x1b[2J");
        for y in 1..=12 {
            frame.extend_from_slice(
                format!("\x1b[{y};1H\x1b[7m row {y:02} \x1b[0m body").as_bytes(),
            );
        }
        frame.extend_from_slice(b"\x1b[5;10H");
        let bytes = round_trip(50, 12, &frame);
        assert!(
            bytes.starts_with(b"\x1b[?1049h"),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn hidden_cursor_and_input_modes_round_trip() {
        let bytes = round_trip(
            40,
            8,
            b"\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[?1hmenu",
        );
        let text = String::from_utf8_lossy(&bytes);
        for mode in [
            "?25l", "?1000h", "?1002h", "?1006h", "?1004h", "?2004h", "?1h",
        ] {
            assert!(text.contains(mode), "{mode} missing in {text:?}");
        }
    }

    #[test]
    fn autowrap_off_and_origin_mode_round_trip() {
        round_trip(
            20,
            5,
            b"\x1b[?7l0123456789012345678901234567890\x1b[?6h\x1b[2;4r\x1b[Hx",
        );
    }

    #[test]
    fn scroll_region_with_content_round_trips() {
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[1;1Hheader\x1b[10;1Hfooter\x1b[2;9r\x1b[2;1H");
        for i in 0..20 {
            input.extend_from_slice(format!("scrolling {i}\r\n").as_bytes());
        }
        let bytes = round_trip(30, 10, &input);
        assert!(
            bytes.windows(6).any(|w| w == b"\x1b[2;9r"),
            "region emitted"
        );
    }

    #[test]
    fn mid_repaint_tui_frame_round_trips() {
        // A ratatui-style incremental repaint: a full frame, then a partial
        // update that moved the cursor around and restyled a few cells, with
        // the terminal left mid-frame (no final cursor park, pen still set).
        let mut input = Vec::new();
        input.extend_from_slice(b"\x1b[?1049h\x1b[?25l\x1b[2J");
        for y in 1..=20 {
            input.extend_from_slice(
                format!(
                    "\x1b[{y};1H\x1b[38;5;{}m│ item {y:02}\x1b[0m   status",
                    100 + y
                )
                .as_bytes(),
            );
        }
        input.extend_from_slice(
            b"\x1b[7;3H\x1b[1;33mITEM 07 (selected)\x1b[12;20H\x1b[48;2;30;30;60m  busy ",
        );
        let mut a = VtTerminal::new(60, 20);
        a.write(&input);
        assert!(!a.mode(25));
        round_trip(60, 20, &input);
    }

    #[test]
    fn synchronized_output_in_progress_is_carried_to_the_client() {
        let bytes = round_trip(20, 4, b"\x1b[?2026h\x1b[2Jhalf");
        assert!(bytes.starts_with(b"\x1b[?2026h"));
    }

    #[test]
    fn pending_wrap_cursor_position_matches() {
        // The pending-wrap flag itself is documented as not reproduced; the
        // visible cursor column must still match.
        let input = b"0123456789";
        let mut a = VtTerminal::new(10, 3);
        a.write(input);
        assert!(a.cursor_pending_wrap());
        round_trip(10, 3, input);
    }

    /// The render scratch is shared per thread: snapshots of two terminals
    /// alternating on one thread must never leak rows across, and repeated
    /// snapshots of one terminal (the incremental path) must track edits.
    #[test]
    fn shared_render_scratch_rebinds_without_cross_talk() {
        let mut a = VtTerminal::new(20, 3);
        let mut b = VtTerminal::new(20, 3);
        a.write(b"AAAA\r\nsecond-a");
        b.write(b"bbbbbbbb\r\n\r\nthird-b");
        for _ in 0..3 {
            let rows_a: Vec<String> = a.viewport_rows(false).into_iter().map(|r| r.text).collect();
            assert_eq!(rows_a[0].trim_end(), "AAAA");
            assert_eq!(rows_a[1].trim_end(), "second-a");
            assert_eq!(rows_a[2].trim_end(), "");
            let rows_b: Vec<String> = b.viewport_rows(false).into_iter().map(|r| r.text).collect();
            assert_eq!(rows_b[0].trim_end(), "bbbbbbbb");
            assert_eq!(rows_b[1].trim_end(), "");
            assert_eq!(rows_b[2].trim_end(), "third-b");
        }
        // Incremental path: same terminal, an edit, then the row moved.
        a.write(b"\x1b[3;1Hnew-third");
        let rows_a: Vec<String> = a.viewport_rows(false).into_iter().map(|r| r.text).collect();
        assert_eq!(rows_a[2].trim_end(), "new-third");
        // A dropped terminal's id is never reused: a fresh terminal after it
        // renders its own (empty) grid, not the stale rows.
        drop(a);
        let mut c = VtTerminal::new(20, 3);
        let rows_c: Vec<String> = c.viewport_rows(false).into_iter().map(|r| r.text).collect();
        assert!(rows_c.iter().all(|r| r.trim_end().is_empty()), "{rows_c:?}");
        release_render_scratch();
        let rows_b: Vec<String> = b.viewport_rows(false).into_iter().map(|r| r.text).collect();
        assert_eq!(rows_b[2].trim_end(), "third-b");
    }

    #[test]
    fn state_snapshot_pairs_offset_with_render() {
        let mut state = TerminalViewportState::new(20, 5);
        state.reset_at_output_offset(20, 5, 1_000, true);
        state.feed(b"abc");
        let (offset, snapshot) = state.snapshot_vt();
        assert_eq!(offset, 1_003);
        assert_eq!((snapshot.cols, snapshot.rows), (20, 5));
        assert!(snapshot.bytes.starts_with(b"abc"));
        assert!(snapshot.bytes.ends_with(b"\x1b[1;4H"));
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: SnapshotVt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snapshot);
    }
}
