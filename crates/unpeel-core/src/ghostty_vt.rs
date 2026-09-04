//! Hand-written FFI bindings for the vendored libghostty-vt static library
//! (`vendor/ghostty-vt/`, see its README for provenance and rebuild steps).
//!
//! Declarations mirror `include/ghostty/vt/*.h` in the ghostty checkout the
//! archive was built from. Only the subset `terminal_viewport.rs` needs is
//! bound. The `layout_matches_type_json` test cross-checks struct layouts
//! against the library's own `ghostty_type_json()` metadata, so a vendored
//! archive bump that changes a struct fails loudly instead of corrupting
//! memory.

#![allow(dead_code)]

use std::os::raw::{c_char, c_int};

pub type GhosttyResult = c_int;
pub const GHOSTTY_SUCCESS: GhosttyResult = 0;
pub const GHOSTTY_OUT_OF_SPACE: GhosttyResult = -3;
pub const GHOSTTY_NO_VALUE: GhosttyResult = -4;

// Opaque handles.
#[repr(C)]
pub struct GhosttyTerminalImpl {
    _private: [u8; 0],
}
pub type GhosttyTerminal = *mut GhosttyTerminalImpl;

#[repr(C)]
pub struct GhosttyRenderStateImpl {
    _private: [u8; 0],
}
pub type GhosttyRenderState = *mut GhosttyRenderStateImpl;

#[repr(C)]
pub struct GhosttyRenderStateRowIteratorImpl {
    _private: [u8; 0],
}
pub type GhosttyRenderStateRowIterator = *mut GhosttyRenderStateRowIteratorImpl;

#[repr(C)]
pub struct GhosttyRenderStateRowCellsImpl {
    _private: [u8; 0],
}
pub type GhosttyRenderStateRowCells = *mut GhosttyRenderStateRowCellsImpl;

#[repr(C)]
pub struct GhosttyFormatterImpl {
    _private: [u8; 0],
}
/// Opaque formatter handle (`typedef struct GhosttyFormatterImpl* GhosttyFormatter`).
pub type GhosttyFormatter = *mut GhosttyFormatterImpl;

/// Opaque cell value (`typedef uint64_t GhosttyCell`).
pub type GhosttyCell = u64;
/// Opaque row value (`typedef uint64_t GhosttyRow`).
pub type GhosttyRow = u64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

// GhosttyStyleColor (tagged union, 8-byte aligned union payload).
pub const GHOSTTY_STYLE_COLOR_NONE: c_int = 0;
pub const GHOSTTY_STYLE_COLOR_PALETTE: c_int = 1;
pub const GHOSTTY_STYLE_COLOR_RGB: c_int = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyStyleColorValue {
    pub palette: u8,
    pub rgb: GhosttyColorRgb,
    pub _padding: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyleColor {
    pub tag: c_int,
    pub value: GhosttyStyleColorValue,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyle {
    pub size: usize,
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: c_int,
}

impl GhosttyStyle {
    pub fn zeroed_sized() -> Self {
        let mut style: Self = unsafe { std::mem::zeroed() };
        style.size = std::mem::size_of::<Self>();
        style
    }
}

// GhosttyFormatterFormat.
pub const GHOSTTY_FORMATTER_FORMAT_PLAIN: c_int = 0;
pub const GHOSTTY_FORMATTER_FORMAT_VT: c_int = 1;
pub const GHOSTTY_FORMATTER_FORMAT_HTML: c_int = 2;

/// Screen-level extras the VT formatter emits after the screen content
/// (`GhosttyFormatterScreenExtra`); every flag is only honoured for
/// `GHOSTTY_FORMATTER_FORMAT_VT`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyFormatterScreenExtra {
    pub size: usize,
    /// Cursor position via CUP.
    pub cursor: bool,
    /// The cursor's active SGR pen.
    pub style: bool,
    /// Active OSC 8 hyperlink.
    pub hyperlink: bool,
    /// DECSCA protection.
    pub protection: bool,
    /// Kitty keyboard protocol flags.
    pub kitty_keyboard: bool,
    /// Charset designations and invocations.
    pub charsets: bool,
}

/// Terminal-level extras (`GhosttyFormatterTerminalExtra`): modes and the
/// palette are emitted BEFORE the content, scrolling region / tabstops /
/// keyboard / pwd after it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyFormatterTerminalExtra {
    pub size: usize,
    /// OSC 4 palette entries.
    pub palette: bool,
    /// Every mode that differs from its default, as CSI h/l.
    pub modes: bool,
    /// DECSTBM / DECSLRM.
    pub scrolling_region: bool,
    /// Clear all tabs then HTS each configured stop.
    pub tabstops: bool,
    /// OSC 7.
    pub pwd: bool,
    /// ModifyOtherKeys.
    pub keyboard: bool,
    pub screen: GhosttyFormatterScreenExtra,
}

/// `GhosttyFormatterTerminalOptions`. `selection` NULL formats the whole
/// active screen (scrollback included).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyFormatterTerminalOptions {
    pub size: usize,
    pub emit: c_int,
    pub unwrap: bool,
    pub trim: bool,
    pub extra: GhosttyFormatterTerminalExtra,
    pub selection: *const std::ffi::c_void,
}

impl GhosttyFormatterTerminalOptions {
    /// Options that reconstruct the active screen as VT bytes on a fresh
    /// terminal: content with per-cell styles, then modes, scrolling region,
    /// tabstops, keyboard flags, charsets, the cursor pen, and the cursor.
    /// The palette and OSC 7 are deliberately NOT emitted — the client
    /// terminal's theme and cwd are its own.
    pub fn vt_state_snapshot() -> Self {
        Self {
            size: std::mem::size_of::<Self>(),
            emit: GHOSTTY_FORMATTER_FORMAT_VT,
            unwrap: true,
            trim: true,
            extra: GhosttyFormatterTerminalExtra {
                size: std::mem::size_of::<GhosttyFormatterTerminalExtra>(),
                palette: false,
                modes: true,
                scrolling_region: true,
                tabstops: true,
                pwd: false,
                keyboard: true,
                screen: GhosttyFormatterScreenExtra {
                    size: std::mem::size_of::<GhosttyFormatterScreenExtra>(),
                    cursor: true,
                    style: true,
                    hyperlink: true,
                    protection: true,
                    kitty_keyboard: true,
                    charsets: true,
                },
            },
            selection: std::ptr::null(),
        }
    }
}

#[repr(C)]
pub struct GhosttyTerminalOptions {
    pub cols: u16,
    pub rows: u16,
    pub max_scrollback: usize,
}

// Scroll viewport tagged union.
pub const GHOSTTY_SCROLL_VIEWPORT_TOP: c_int = 0;
pub const GHOSTTY_SCROLL_VIEWPORT_BOTTOM: c_int = 1;
pub const GHOSTTY_SCROLL_VIEWPORT_DELTA: c_int = 2;
pub const GHOSTTY_SCROLL_VIEWPORT_ROW: c_int = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyTerminalScrollViewportValue {
    pub delta: isize,
    pub row: usize,
    pub _padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalScrollViewport {
    pub tag: c_int,
    pub value: GhosttyTerminalScrollViewportValue,
}

impl GhosttyTerminalScrollViewport {
    pub fn bottom() -> Self {
        Self {
            tag: GHOSTTY_SCROLL_VIEWPORT_BOTTOM,
            value: GhosttyTerminalScrollViewportValue { _padding: [0; 2] },
        }
    }

    pub fn row(row: usize) -> Self {
        let mut value = GhosttyTerminalScrollViewportValue { _padding: [0; 2] };
        value.row = row;
        Self {
            tag: GHOSTTY_SCROLL_VIEWPORT_ROW,
            value,
        }
    }
}

/// Caller-provided byte buffer for GRAPHEMES_UTF8 reads.
#[repr(C)]
pub struct GhosttyBuffer {
    pub ptr: *mut u8,
    pub cap: usize,
    pub len: usize,
}

// GhosttyTerminalData (ghostty_terminal_get keys).
pub const GHOSTTY_TERMINAL_DATA_COLS: c_int = 1;
pub const GHOSTTY_TERMINAL_DATA_ROWS: c_int = 2;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_X: c_int = 3;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_Y: c_int = 4;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP: c_int = 5;
pub const GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN: c_int = 6;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE: c_int = 7;
pub const GHOSTTY_TERMINAL_DATA_TOTAL_ROWS: c_int = 14;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS: c_int = 15;

pub const GHOSTTY_TERMINAL_SCREEN_PRIMARY: c_int = 0;
pub const GHOSTTY_TERMINAL_SCREEN_ALTERNATE: c_int = 1;

// GhosttyRenderStateData (ghostty_render_state_get keys).
pub const GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR: c_int = 4;

// GhosttyRenderStateRowData (ghostty_render_state_row_get keys).
pub const GHOSTTY_RENDER_STATE_ROW_DATA_RAW: c_int = 2;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_CELLS: c_int = 3;

// GhosttyRowData (ghostty_row_get keys).
pub const GHOSTTY_ROW_DATA_WRAP: c_int = 1;

// GhosttyRenderStateRowCellsData (ghostty_render_state_row_cells_get keys).
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW: c_int = 1;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE: c_int = 2;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR: c_int = 5;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING: c_int = 8;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8: c_int = 9;

// GhosttyCellData (ghostty_cell_get keys) and GhosttyCellWide values.
pub const GHOSTTY_CELL_DATA_WIDE: c_int = 3;
pub const GHOSTTY_CELL_WIDE_SPACER_TAIL: c_int = 2;
pub const GHOSTTY_CELL_WIDE_SPACER_HEAD: c_int = 3;

extern "C" {
    pub fn ghostty_terminal_new(
        allocator: *const std::ffi::c_void,
        terminal: *mut GhosttyTerminal,
        options: GhosttyTerminalOptions,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_free(terminal: GhosttyTerminal);
    pub fn ghostty_terminal_vt_write(terminal: GhosttyTerminal, data: *const u8, len: usize);
    pub fn ghostty_terminal_resize(
        terminal: GhosttyTerminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_scroll_viewport(
        terminal: GhosttyTerminal,
        behavior: GhosttyTerminalScrollViewport,
    );
    pub fn ghostty_terminal_get(
        terminal: GhosttyTerminal,
        data: c_int,
        out: *mut std::ffi::c_void,
    ) -> GhosttyResult;
    /// Query a terminal mode ghostty tracks from the fed byte stream. DEC
    /// private modes use their raw number — 2026 is synchronized output.
    pub fn ghostty_terminal_mode_get(
        terminal: GhosttyTerminal,
        mode: u16,
        out_value: *mut bool,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_new(
        allocator: *const std::ffi::c_void,
        state: *mut GhosttyRenderState,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_free(state: GhosttyRenderState);
    pub fn ghostty_render_state_update(
        state: GhosttyRenderState,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_get(
        state: GhosttyRenderState,
        data: c_int,
        out: *mut std::ffi::c_void,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_row_iterator_new(
        allocator: *const std::ffi::c_void,
        out_iterator: *mut GhosttyRenderStateRowIterator,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_iterator_free(iterator: GhosttyRenderStateRowIterator);
    pub fn ghostty_render_state_row_iterator_next(iterator: GhosttyRenderStateRowIterator) -> bool;
    pub fn ghostty_render_state_row_get(
        iterator: GhosttyRenderStateRowIterator,
        data: c_int,
        out: *mut std::ffi::c_void,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_row_cells_new(
        allocator: *const std::ffi::c_void,
        out_cells: *mut GhosttyRenderStateRowCells,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_free(cells: GhosttyRenderStateRowCells);
    pub fn ghostty_render_state_row_cells_next(cells: GhosttyRenderStateRowCells) -> bool;
    pub fn ghostty_render_state_row_cells_get(
        cells: GhosttyRenderStateRowCells,
        data: c_int,
        out: *mut std::ffi::c_void,
    ) -> GhosttyResult;

    pub fn ghostty_cell_get(
        cell: GhosttyCell,
        data: c_int,
        out: *mut std::ffi::c_void,
    ) -> GhosttyResult;
    pub fn ghostty_row_get(
        row: GhosttyRow,
        data: c_int,
        out: *mut std::ffi::c_void,
    ) -> GhosttyResult;

    pub fn ghostty_type_json() -> *const c_char;

    /// Formatter over a terminal's active screen. `options` is passed by
    /// value (the C signature takes the struct, not a pointer).
    pub fn ghostty_formatter_terminal_new(
        allocator: *const std::ffi::c_void,
        formatter: *mut GhosttyFormatter,
        terminal: GhosttyTerminal,
        options: GhosttyFormatterTerminalOptions,
    ) -> GhosttyResult;
    /// Format into a caller buffer. `GHOSTTY_OUT_OF_SPACE` reports the
    /// required size in `out_written` (a NULL/zero buffer is a size query).
    pub fn ghostty_formatter_format_buf(
        formatter: GhosttyFormatter,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_free(formatter: GhosttyFormatter);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(json: &serde_json::Value, name: &str) -> (usize, usize) {
        let entry = &json[name];
        (
            entry["size"].as_u64().expect("size") as usize,
            entry["align"].as_u64().expect("align") as usize,
        )
    }

    /// Cross-check the hand-written struct declarations against the
    /// library's own layout metadata. Fails loudly on a vendored-archive
    /// bump that changes any struct we bind.
    #[test]
    fn layout_matches_type_json() {
        let raw = unsafe { std::ffi::CStr::from_ptr(ghostty_type_json()) };
        let json: serde_json::Value =
            serde_json::from_str(raw.to_str().expect("utf8")).expect("type json parses");

        for (name, size, align) in [
            (
                "GhosttyStyle",
                std::mem::size_of::<GhosttyStyle>(),
                std::mem::align_of::<GhosttyStyle>(),
            ),
            (
                "GhosttyStyleColor",
                std::mem::size_of::<GhosttyStyleColor>(),
                std::mem::align_of::<GhosttyStyleColor>(),
            ),
            (
                "GhosttyTerminalOptions",
                std::mem::size_of::<GhosttyTerminalOptions>(),
                std::mem::align_of::<GhosttyTerminalOptions>(),
            ),
            (
                "GhosttyTerminalScrollViewport",
                std::mem::size_of::<GhosttyTerminalScrollViewport>(),
                std::mem::align_of::<GhosttyTerminalScrollViewport>(),
            ),
            (
                "GhosttyBuffer",
                std::mem::size_of::<GhosttyBuffer>(),
                std::mem::align_of::<GhosttyBuffer>(),
            ),
            (
                "GhosttyColorRgb",
                std::mem::size_of::<GhosttyColorRgb>(),
                std::mem::align_of::<GhosttyColorRgb>(),
            ),
            (
                "GhosttyFormatterScreenExtra",
                std::mem::size_of::<GhosttyFormatterScreenExtra>(),
                std::mem::align_of::<GhosttyFormatterScreenExtra>(),
            ),
            (
                "GhosttyFormatterTerminalExtra",
                std::mem::size_of::<GhosttyFormatterTerminalExtra>(),
                std::mem::align_of::<GhosttyFormatterTerminalExtra>(),
            ),
            (
                "GhosttyFormatterTerminalOptions",
                std::mem::size_of::<GhosttyFormatterTerminalOptions>(),
                std::mem::align_of::<GhosttyFormatterTerminalOptions>(),
            ),
        ] {
            let (expected_size, expected_align) = layout(&json, name);
            assert_eq!(size, expected_size, "{name} size");
            assert_eq!(align, expected_align, "{name} align");
        }
    }
}
