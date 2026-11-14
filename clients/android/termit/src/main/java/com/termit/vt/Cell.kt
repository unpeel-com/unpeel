package com.termit.vt

import com.termit.ui.AnsiColors

data class Cell(
    var char: Char = ' ',
    var fg: Int = CellStyle.DEFAULT_FG,
    var bg: Int = CellStyle.DEFAULT_BG,
    var bold: Boolean = false,
    var underline: Boolean = false,
    var inverse: Boolean = false,
    var dim: Boolean = false,
) {
    fun reset() {
        char = ' '
        fg = CellStyle.DEFAULT_FG
        bg = CellStyle.DEFAULT_BG
        bold = false
        underline = false
        inverse = false
        dim = false
    }

    fun copyFrom(other: Cell) {
        char = other.char
        fg = other.fg
        bg = other.bg
        bold = other.bold
        underline = other.underline
        inverse = other.inverse
        dim = other.dim
    }

    fun applyStyle(style: CellStyle) {
        fg = style.fg
        bg = style.bg
        bold = style.bold
        underline = style.underline
        inverse = style.inverse
        dim = style.dim
    }
}

data class CellStyle(
    var fg: Int = DEFAULT_FG,
    var bg: Int = DEFAULT_BG,
    var bold: Boolean = false,
    var underline: Boolean = false,
    var inverse: Boolean = false,
    var dim: Boolean = false,
) {
    fun reset() {
        fg = DEFAULT_FG
        bg = DEFAULT_BG
        bold = false
        underline = false
        inverse = false
        dim = false
    }

    companion object {
        /** Termux COLOR_INDEX_FOREGROUND / BACKGROUND. */
        const val DEFAULT_FG = AnsiColors.INDEX_FG
        const val DEFAULT_BG = AnsiColors.INDEX_BG
    }
}
