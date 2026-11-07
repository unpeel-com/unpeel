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

// GhosttyStyleColor: a byte tag and a three-byte color payload.
pub const GHOSTTY_STYLE_COLOR_NONE: u8 = 0;
pub const GHOSTTY_STYLE_COLOR_PALETTE: u8 = 1;
pub const GHOSTTY_STYLE_COLOR_RGB: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyStyleColorValue {
    pub palette: u8,
    pub rgb: GhosttyColorRgb,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyleColor {
    pub tag: u8,
    pub value: GhosttyStyleColorValue,
}

/// Fixed 16-byte style in the stable 1.0 C ABI.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GhosttyStyle {
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub flags: u16,
    pub reserved: u16,
}

impl Default for GhosttyStyleColor {
    fn default() -> Self {
        Self {
            tag: GHOSTTY_STYLE_COLOR_NONE,
            value: GhosttyStyleColorValue {
                rgb: GhosttyColorRgb::default(),
            },
        }
    }
}

pub const GHOSTTY_STYLE_FLAG_BOLD: u16 = 1 << 0;
pub const GHOSTTY_STYLE_FLAG_INVERSE: u16 = 1 << 4;

// GhosttyFormatterOption. Values are copied by ghostty_formatter_set.
pub const GHOSTTY_FORMATTER_OPT_EMIT: c_int = 1;
pub const GHOSTTY_FORMATTER_OPT_UNWRAP: c_int = 2;
pub const GHOSTTY_FORMATTER_OPT_TRIM: c_int = 3;
pub const GHOSTTY_FORMATTER_OPT_EXTRAS: c_int = 5;
pub const GHOSTTY_FORMATTER_FORMAT_VT: c_int = 1;

/// Restore modes, margins, tabs, keyboard, cursor, pen, hyperlink,
/// protection and charsets. Leave palette and cwd to the Controller.
pub const GHOSTTY_FORMATTER_EXTRAS_SNAPSHOT: u32 = 0xFFF & !((1 << 0) | (1 << 4));

// GhosttyTerminalOption (ghostty_terminal_set keys).
/// Scrollback page-storage budget in bytes (`size_t*`); pruned at page
/// granularity. Zero disables scrollback.
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES: c_int = 28;

/// Packed mode id (`typedef uint16_t GhosttyMode`): the numeric value in the
/// low 15 bits, the ANSI flag in the top bit (clear for DEC private modes).
pub type GhosttyMode = u16;

/// `ghostty_mode_new` is a static inline in `modes.h`, so it is not exported
/// from the archive.
pub const fn ghostty_mode_new(value: u16, ansi: bool) -> GhosttyMode {
    (value & 0x7FFF) | ((ansi as u16) << 15)
}

/// In/out parameter for `GHOSTTY_TERMINAL_DATA_MODE`: set `mode`, read
/// `value` back. Frozen layout upstream.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyTerminalModeConfig {
    pub mode: GhosttyMode,
    pub value: bool,
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
    pub delta_prompt: isize,
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
pub const GHOSTTY_TERMINAL_DATA_MODE: c_int = 37;

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
        cols: u16,
        rows: u16,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_set(
        terminal: GhosttyTerminal,
        option: c_int,
        value: *const std::ffi::c_void,
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
    ) -> GhosttyResult;
    pub fn ghostty_terminal_get(
        terminal: GhosttyTerminal,
        data: c_int,
        out: *mut std::ffi::c_void,
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

    pub fn ghostty_version() -> *const c_char;
    pub fn ghostty_build_info(data: c_int, out: *mut std::ffi::c_void) -> GhosttyResult;

    /// Formatter over a terminal's active screen; configure it with set().
    pub fn ghostty_formatter_terminal_new(
        allocator: *const std::ffi::c_void,
        formatter: *mut GhosttyFormatter,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_set(
        formatter: GhosttyFormatter,
        option: c_int,
        value: *const std::ffi::c_void,
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

    #[test]
    fn stable_release_and_required_features() {
        let version = unsafe { std::ffi::CStr::from_ptr(ghostty_version()) };
        assert_eq!(version.to_str().unwrap(), "1.0.0");
        // SIMD, snapshot, formatter, render-state and grid introspection.
        for feature in [1, 11, 12, 15, 18] {
            let mut enabled = false;
            assert_eq!(
                unsafe { ghostty_build_info(feature, &mut enabled as *mut _ as *mut _) },
                GHOSTTY_SUCCESS
            );
            assert!(enabled, "required libghostty feature {feature}");
        }
    }

    fn layout(json: &serde_json::Value, name: &str) -> (usize, usize) {
        let entry = &json["types"][name];
        (
            entry["size"].as_u64().expect("size") as usize,
            entry["align"].as_u64().expect("align") as usize,
        )
    }

    #[test]
    fn enum_values_match_type_json() {
        let raw = unsafe { std::ffi::CStr::from_ptr(ghostty_type_json()) };
        let json: serde_json::Value = serde_json::from_str(raw.to_str().unwrap()).unwrap();
        for (flag, field) in [
            (GHOSTTY_STYLE_FLAG_BOLD, "bold"),
            (GHOSTTY_STYLE_FLAG_INVERSE, "inverse"),
        ] {
            let bit = json["types"]["GhosttyStyleFlags"]["bits"][field]["lsb"]
                .as_u64()
                .unwrap();
            assert_eq!(u64::from(flag), 1 << bit, "style flag {field}");
        }
        for (ty, key, value) in [
            (
                "GhosttyTerminalScrollViewportTag",
                "TOP",
                GHOSTTY_SCROLL_VIEWPORT_TOP as i64,
            ),
            (
                "GhosttyTerminalScrollViewportTag",
                "BOTTOM",
                GHOSTTY_SCROLL_VIEWPORT_BOTTOM as i64,
            ),
            (
                "GhosttyTerminalScrollViewportTag",
                "DELTA",
                GHOSTTY_SCROLL_VIEWPORT_DELTA as i64,
            ),
            (
                "GhosttyTerminalScrollViewportTag",
                "ROW",
                GHOSTTY_SCROLL_VIEWPORT_ROW as i64,
            ),
            ("GhosttyResult", "SUCCESS", GHOSTTY_SUCCESS as i64),
            ("GhosttyResult", "OUT_OF_SPACE", GHOSTTY_OUT_OF_SPACE as i64),
            ("GhosttyResult", "NO_VALUE", GHOSTTY_NO_VALUE as i64),
            (
                "GhosttyStyleColorTag",
                "NONE",
                GHOSTTY_STYLE_COLOR_NONE as i64,
            ),
            (
                "GhosttyStyleColorTag",
                "PALETTE",
                GHOSTTY_STYLE_COLOR_PALETTE as i64,
            ),
            (
                "GhosttyStyleColorTag",
                "RGB",
                GHOSTTY_STYLE_COLOR_RGB as i64,
            ),
            (
                "GhosttyFormatterOption",
                "EMIT",
                GHOSTTY_FORMATTER_OPT_EMIT as i64,
            ),
            (
                "GhosttyFormatterOption",
                "UNWRAP",
                GHOSTTY_FORMATTER_OPT_UNWRAP as i64,
            ),
            (
                "GhosttyFormatterOption",
                "TRIM",
                GHOSTTY_FORMATTER_OPT_TRIM as i64,
            ),
            (
                "GhosttyFormatterOption",
                "EXTRAS",
                GHOSTTY_FORMATTER_OPT_EXTRAS as i64,
            ),
            (
                "GhosttyFormatterFormat",
                "VT",
                GHOSTTY_FORMATTER_FORMAT_VT as i64,
            ),
            (
                "GhosttyTerminalOption",
                "SCROLLBACK_MAX_BYTES",
                GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES as i64,
            ),
            (
                "GhosttyTerminalData",
                "COLS",
                GHOSTTY_TERMINAL_DATA_COLS as i64,
            ),
            (
                "GhosttyTerminalData",
                "ROWS",
                GHOSTTY_TERMINAL_DATA_ROWS as i64,
            ),
            (
                "GhosttyTerminalData",
                "CURSOR_X",
                GHOSTTY_TERMINAL_DATA_CURSOR_X as i64,
            ),
            (
                "GhosttyTerminalData",
                "CURSOR_Y",
                GHOSTTY_TERMINAL_DATA_CURSOR_Y as i64,
            ),
            (
                "GhosttyTerminalData",
                "CURSOR_PENDING_WRAP",
                GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP as i64,
            ),
            (
                "GhosttyTerminalData",
                "ACTIVE_SCREEN",
                GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN as i64,
            ),
            (
                "GhosttyTerminalData",
                "CURSOR_VISIBLE",
                GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE as i64,
            ),
            (
                "GhosttyTerminalData",
                "TOTAL_ROWS",
                GHOSTTY_TERMINAL_DATA_TOTAL_ROWS as i64,
            ),
            (
                "GhosttyTerminalData",
                "SCROLLBACK_ROWS",
                GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS as i64,
            ),
            (
                "GhosttyTerminalData",
                "MODE",
                GHOSTTY_TERMINAL_DATA_MODE as i64,
            ),
            (
                "GhosttyTerminalScreen",
                "PRIMARY",
                GHOSTTY_TERMINAL_SCREEN_PRIMARY as i64,
            ),
            (
                "GhosttyTerminalScreen",
                "ALTERNATE",
                GHOSTTY_TERMINAL_SCREEN_ALTERNATE as i64,
            ),
            (
                "GhosttyRenderStateData",
                "ROW_ITERATOR",
                GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR as i64,
            ),
            (
                "GhosttyRenderStateRowData",
                "RAW",
                GHOSTTY_RENDER_STATE_ROW_DATA_RAW as i64,
            ),
            (
                "GhosttyRenderStateRowData",
                "CELLS",
                GHOSTTY_RENDER_STATE_ROW_DATA_CELLS as i64,
            ),
            ("GhosttyRowData", "WRAP", GHOSTTY_ROW_DATA_WRAP as i64),
            (
                "GhosttyRenderStateRowCellsData",
                "RAW",
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW as i64,
            ),
            (
                "GhosttyRenderStateRowCellsData",
                "STYLE",
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE as i64,
            ),
            (
                "GhosttyRenderStateRowCellsData",
                "BG_COLOR",
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR as i64,
            ),
            (
                "GhosttyRenderStateRowCellsData",
                "HAS_STYLING",
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING as i64,
            ),
            (
                "GhosttyRenderStateRowCellsData",
                "GRAPHEMES_UTF8",
                GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8 as i64,
            ),
            ("GhosttyCellData", "WIDE", GHOSTTY_CELL_DATA_WIDE as i64),
            (
                "GhosttyCellWide",
                "SPACER_TAIL",
                GHOSTTY_CELL_WIDE_SPACER_TAIL as i64,
            ),
            (
                "GhosttyCellWide",
                "SPACER_HEAD",
                GHOSTTY_CELL_WIDE_SPACER_HEAD as i64,
            ),
        ] {
            assert_eq!(
                json["types"][ty]["values"][key].as_i64(),
                Some(value),
                "{ty}.{key}"
            );
        }
    }

    /// Cross-check the hand-written struct declarations against the
    /// library's own layout metadata. Fails loudly on a vendored-archive
    /// bump that changes any struct we bind.
    #[test]
    fn layout_matches_type_json() {
        let raw = unsafe { std::ffi::CStr::from_ptr(ghostty_type_json()) };
        let json: serde_json::Value =
            serde_json::from_str(raw.to_str().expect("utf8")).expect("type json parses");

        for (ty, field, offset) in [
            (
                "GhosttyStyleColor",
                "value",
                std::mem::offset_of!(GhosttyStyleColor, value),
            ),
            (
                "GhosttyStyle",
                "fg_color",
                std::mem::offset_of!(GhosttyStyle, fg_color),
            ),
            (
                "GhosttyStyle",
                "bg_color",
                std::mem::offset_of!(GhosttyStyle, bg_color),
            ),
            (
                "GhosttyStyle",
                "underline_color",
                std::mem::offset_of!(GhosttyStyle, underline_color),
            ),
            (
                "GhosttyStyle",
                "flags",
                std::mem::offset_of!(GhosttyStyle, flags),
            ),
            (
                "GhosttyStyle",
                "reserved",
                std::mem::offset_of!(GhosttyStyle, reserved),
            ),
        ] {
            assert_eq!(
                json["types"][ty]["fields"][field]["offset"].as_u64(),
                Some(offset as u64),
                "{ty}.{field}"
            );
        }

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
                "GhosttyTerminalModeConfig",
                std::mem::size_of::<GhosttyTerminalModeConfig>(),
                std::mem::align_of::<GhosttyTerminalModeConfig>(),
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
        ] {
            let (expected_size, expected_align) = layout(&json, name);
            assert_eq!(size, expected_size, "{name} size");
            assert_eq!(align, expected_align, "{name} align");
        }
    }
}
