package com.termit.vt

data class Cell(
    var char: Char = ' ',
    var fg: Int = CellStyle.DEFAULT_FG,
    var bg: Int = CellStyle.DEFAULT_BG,
    var bold: Boolean = false,
    var underline: Boolean = false,
    var inverse: Boolean = false,
) {
    fun reset() {
        char = ' '
        fg = CellStyle.DEFAULT_FG
        bg = CellStyle.DEFAULT_BG
        bold = false
        underline = false
        inverse = false
    }

    fun copyFrom(other: Cell) {
        char = other.char
        fg = other.fg
        bg = other.bg
        bold = other.bold
        underline = other.underline
        inverse = other.inverse
    }

    fun applyStyle(style: CellStyle) {
        fg = style.fg
        bg = style.bg
        bold = style.bold
        underline = style.underline
        inverse = style.inverse
    }
}

data class CellStyle(
    var fg: Int = DEFAULT_FG,
    var bg: Int = DEFAULT_BG,
    var bold: Boolean = false,
    var underline: Boolean = false,
    var inverse: Boolean = false,
) {
    fun reset() {
        fg = DEFAULT_FG
        bg = DEFAULT_BG
        bold = false
        underline = false
        inverse = false
    }

    companion object {
        const val DEFAULT_FG = 7
        const val DEFAULT_BG = 0
    }
}
