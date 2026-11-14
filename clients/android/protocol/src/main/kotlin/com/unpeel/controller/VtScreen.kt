package com.unpeel.controller

/**
 * Protocol-module VT harness for unit tests. The phone UI renders through
 * `:termit` (`HostTerminal`); keep this class for wire/parser coverage only.
 * The keyboard must not resize a Host PTY — that rule lives on the app fit path.
 */
class VtScreen(cols: Int, rows: Int) {
    var cols = cols.coerceIn(2, 300)
        private set
    var rows = rows.coerceIn(2, 200)
        private set

    data class Cell(val ch: Char, val fg: Int, val bg: Int, val bold: Boolean)

    private class Grid(cols: Int, rows: Int) {
        var cells = Array(rows) { Array(cols) { Cell(' ', DEFAULT_FG, DEFAULT_BG, false) } }
        var cursorRow = 0
        var cursorCol = 0
        var savedRow = 0
        var savedCol = 0
        var scrollTop = 0
        var scrollBottom = rows - 1
    }

    private var normal = Grid(this.cols, this.rows)
    private var alt = Grid(this.cols, this.rows)
    private var onAlt = false
    private val grid: Grid get() = if (onAlt) alt else normal
    private val history = ArrayDeque<Array<Cell>>()

    private var fg = DEFAULT_FG
    private var bg = DEFAULT_BG
    private var bold = false
    private var utf8 = ByteArray(0)

    private enum class Mode { GROUND, ESC, CSI, OSC }

    private var mode = Mode.GROUND
    private val csi = StringBuilder()
    private var csiPrivate = false

    fun resize(columns: Int, lines: Int) {
        cols = columns.coerceIn(2, 300)
        rows = lines.coerceIn(2, 200)
        normal = Grid(cols, rows)
        alt = Grid(cols, rows)
        onAlt = false
        history.clear()
        resetStyle()
        mode = Mode.GROUND
        utf8 = ByteArray(0)
    }

    fun reset() {
        resize(cols, rows)
    }

    fun feed(bytes: ByteArray) {
        val all = utf8 + bytes
        var index = 0
        while (index < all.size) {
            val byte = all[index]
            if (mode == Mode.GROUND && (byte.toInt() and 0xff) >= 0x80) {
                val length = utf8Length(byte)
                if (index + length > all.size) {
                    utf8 = all.copyOfRange(index, all.size)
                    return
                }
                val text = all.copyOfRange(index, index + length).toString(Charsets.UTF_8)
                put(text.firstOrNull() ?: '\uFFFD')
                index += length
                continue
            }
            consume(byte.toInt() and 0xff)
            index += 1
        }
        utf8 = ByteArray(0)
    }

    fun snapshot(): List<List<Cell>> = grid.cells.map { row -> row.toList() }

    /** Lines that scrolled off the top of the primary screen. The alt screen does not record. */
    fun scrollback(): List<List<Cell>> = history.map { row -> row.toList() }

    fun cursor(): Pair<Int, Int> = grid.cursorRow to grid.cursorCol

    private fun consume(code: Int) {
        when (mode) {
            Mode.GROUND -> when (code) {
                0x08 -> backspace()
                0x09 -> tab()
                0x0A, 0x0B, 0x0C -> lineFeed()
                0x0D -> grid.cursorCol = 0
                0x18, 0x1A -> mode = Mode.GROUND
                0x1B -> mode = Mode.ESC
                else -> if (code >= 0x20) put(code.toChar())
            }
            Mode.ESC -> {
                mode = Mode.GROUND
                when (code) {
                    '['.code -> {
                        mode = Mode.CSI
                        csi.clear()
                        csiPrivate = false
                    }
                    ']'.code -> mode = Mode.OSC
                    '7'.code -> {
                        grid.savedRow = grid.cursorRow
                        grid.savedCol = grid.cursorCol
                    }
                    '8'.code -> {
                        grid.cursorRow = grid.savedRow
                        grid.cursorCol = grid.savedCol
                    }
                    'c'.code -> reset()
                    'D'.code -> lineFeed()
                    'M'.code -> reverseIndex()
                    'E'.code -> {
                        grid.cursorCol = 0
                        lineFeed()
                    }
                }
            }
            Mode.CSI -> when {
                code == '?'.code && csi.isEmpty() -> csiPrivate = true
                code in 0x30..0x3F -> csi.append(code.toChar())
                code in 0x20..0x2F -> csi.append(code.toChar())
                code in 0x40..0x7E -> {
                    dispatchCsi(code.toChar(), csi.toString(), csiPrivate)
                    mode = Mode.GROUND
                }
                else -> mode = Mode.GROUND
            }
            Mode.OSC -> if (code == 0x07 || code == 0x1B) {
                // BEL ends OSC. ESC starts the ST terminator; the next byte is ignored in GROUND via ESC state only if we stay. 0x1B here ends and we drop the following '\\' by going ESC only when it isn't ST. Treat 0x1B as end and enter ESC so '\\' is a no-op ESC final.
                mode = if (code == 0x1B) Mode.ESC else Mode.GROUND
            }
        }
    }

    private fun dispatchCsi(final: Char, body: String, private: Boolean) {
        val params = body.split(';').map { it.trim().trim('?').toIntOrNull() ?: 0 }
        fun p(index: Int, default: Int) = params.getOrNull(index)?.takeIf { it != 0 } ?: default
        if (private) {
            when (final) {
                'h' -> if (params.firstOrNull() == 1049) enterAlt()
                'l' -> if (params.firstOrNull() == 1049) leaveAlt()
            }
            return
        }
        when (final) {
            'A' -> grid.cursorRow = (grid.cursorRow - p(0, 1)).coerceAtLeast(0)
            'B' -> grid.cursorRow = (grid.cursorRow + p(0, 1)).coerceAtMost(rows - 1)
            'C' -> grid.cursorCol = (grid.cursorCol + p(0, 1)).coerceAtMost(cols - 1)
            'D' -> grid.cursorCol = (grid.cursorCol - p(0, 1)).coerceAtLeast(0)
            'G' -> grid.cursorCol = (p(0, 1) - 1).coerceIn(0, cols - 1)
            'H', 'f' -> {
                grid.cursorRow = (p(0, 1) - 1).coerceIn(0, rows - 1)
                grid.cursorCol = (p(1, 1) - 1).coerceIn(0, cols - 1)
            }
            'J' -> eraseDisplay(params.firstOrNull() ?: 0)
            'K' -> eraseLine(params.firstOrNull() ?: 0)
            'm' -> sgr(if (body.isEmpty()) listOf(0) else params)
            'r' -> {
                val top = (p(0, 1) - 1).coerceIn(0, rows - 1)
                val bottom = (params.getOrNull(1)?.takeIf { it != 0 } ?: rows) - 1
                grid.scrollTop = top
                grid.scrollBottom = bottom.coerceIn(top, rows - 1)
            }
            'L' -> insertLines(p(0, 1))
            'M' -> deleteLines(p(0, 1))
        }
    }

    private fun enterAlt() {
        if (onAlt) return
        onAlt = true
        alt = Grid(cols, rows)
    }

    private fun leaveAlt() {
        onAlt = false
    }

    private fun sgr(params: List<Int>) {
        var index = 0
        val values = if (params.isEmpty()) listOf(0) else params
        while (index < values.size) {
            when (val value = values[index]) {
                0 -> resetStyle()
                1 -> bold = true
                22 -> bold = false
                39 -> fg = DEFAULT_FG
                49 -> bg = DEFAULT_BG
                in 30..37 -> fg = ANSI[value - 30]
                in 40..47 -> bg = ANSI[value - 40]
                in 90..97 -> fg = BRIGHT[value - 90]
                in 100..107 -> bg = BRIGHT[value - 100]
                38, 48 -> {
                    val color = when (values.getOrNull(index + 1)) {
                        5 -> {
                            index += 2
                            cube(values.getOrNull(index) ?: 0)
                        }
                        2 -> {
                            val red = values.getOrNull(index + 2) ?: 0
                            val green = values.getOrNull(index + 3) ?: 0
                            val blue = values.getOrNull(index + 4) ?: 0
                            index += 4
                            (red shl 16) or (green shl 8) or blue
                        }
                        else -> null
                    }
                    if (color != null) {
                        if (value == 38) fg = color else bg = color
                    }
                }
            }
            index += 1
        }
    }

    private fun put(ch: Char) {
        val width = if (ch.isWide()) 2 else 1
        if (grid.cursorCol + width > cols) {
            grid.cursorCol = 0
            lineFeed()
        }
        if (grid.cursorRow !in 0 until rows || grid.cursorCol >= cols) return
        grid.cells[grid.cursorRow][grid.cursorCol] = Cell(ch, fg, bg, bold)
        if (width == 2 && grid.cursorCol + 1 < cols) {
            grid.cells[grid.cursorRow][grid.cursorCol + 1] = Cell('\u0000', fg, bg, false)
        }
        grid.cursorCol += width
    }

    private fun backspace() {
        if (grid.cursorCol > 0) grid.cursorCol -= 1
    }

    private fun tab() {
        val next = ((grid.cursorCol / 8) + 1) * 8
        grid.cursorCol = next.coerceAtMost(cols - 1)
    }

    private fun lineFeed() {
        if (grid.cursorRow == grid.scrollBottom) scrollUp() else if (grid.cursorRow < rows - 1) grid.cursorRow += 1
    }

    private fun reverseIndex() {
        if (grid.cursorRow == grid.scrollTop) scrollDown() else if (grid.cursorRow > 0) grid.cursorRow -= 1
    }

    private fun scrollUp() {
        val cells = grid.cells
        if (!onAlt && grid.scrollTop == 0) {
            history.addLast(cells[0].copyOf())
            while (history.size > SCROLLBACK) history.removeFirst()
        }
        for (row in grid.scrollTop until grid.scrollBottom) cells[row] = cells[row + 1]
        cells[grid.scrollBottom] = blankRow()
    }

    private fun scrollDown() {
        val cells = grid.cells
        for (row in grid.scrollBottom downTo grid.scrollTop + 1) cells[row] = cells[row - 1]
        cells[grid.scrollTop] = blankRow()
    }

    private fun insertLines(count: Int) {
        repeat(count.coerceAtLeast(1)) { scrollDown() }
    }

    private fun deleteLines(count: Int) {
        repeat(count.coerceAtLeast(1)) { scrollUp() }
    }

    private fun eraseDisplay(mode: Int) {
        when (mode) {
            0 -> {
                eraseLine(0)
                for (row in grid.cursorRow + 1 until rows) grid.cells[row] = blankRow()
            }
            1 -> {
                eraseLine(1)
                for (row in 0 until grid.cursorRow) grid.cells[row] = blankRow()
            }
            else -> for (row in 0 until rows) grid.cells[row] = blankRow()
        }
    }

    private fun eraseLine(mode: Int) {
        val row = grid.cursorRow
        if (row !in 0 until rows) return
        val range = when (mode) {
            1 -> 0..grid.cursorCol
            2 -> 0 until cols
            else -> grid.cursorCol until cols
        }
        for (col in range) {
            if (col in 0 until cols) grid.cells[row][col] = Cell(' ', fg, bg, false)
        }
    }

    private fun blankRow() = Array(cols) { Cell(' ', DEFAULT_FG, DEFAULT_BG, false) }

    private fun resetStyle() {
        fg = DEFAULT_FG
        bg = DEFAULT_BG
        bold = false
    }

    private fun utf8Length(lead: Byte): Int {
        val value = lead.toInt() and 0xff
        return when {
            value and 0xE0 == 0xC0 -> 2
            value and 0xF0 == 0xE0 -> 3
            value and 0xF8 == 0xF0 -> 4
            else -> 1
        }
    }

    companion object {
        const val DEFAULT_FG = 0xD0D0D0
        const val DEFAULT_BG = 0x1E1E1E
        private const val SCROLLBACK = 1000
        private val ANSI = intArrayOf(
            0x1E1E1E, 0xCD3131, 0x0DBC79, 0xE5E510, 0x2472C8, 0xBC3FBC, 0x11A8CD, 0xE5E5E5,
        )
        private val BRIGHT = intArrayOf(
            0x666666, 0xF14C4C, 0x23D18B, 0xF5F543, 0x3B8EEA, 0xD670D6, 0x29B8DB, 0xFFFFFF,
        )

        private fun cube(index: Int): Int {
            if (index < 16) return if (index < 8) ANSI[index] else BRIGHT[index - 8]
            if (index >= 232) {
                val gray = 8 + (index - 232) * 10
                return (gray shl 16) or (gray shl 8) or gray
            }
            val color = index - 16
            val levels = intArrayOf(0, 95, 135, 175, 215, 255)
            val red = levels[color / 36]
            val green = levels[(color / 6) % 6]
            val blue = levels[color % 6]
            return (red shl 16) or (green shl 8) or blue
        }
    }
}

/** East-Asian wide and common emoji occupy two cells. A continuation cell is `\u0000`. */
private fun Char.isWide(): Boolean {
    val code = code
    return code in 0x1100..0x115F ||
        code in 0x2329..0x232A ||
        code in 0x2E80..0xA4CF ||
        code in 0xAC00..0xD7A3 ||
        code in 0xF900..0xFAFF ||
        code in 0xFE10..0xFE19 ||
        code in 0xFE30..0xFE6F ||
        code in 0xFF00..0xFF60 ||
        code in 0xFFE0..0xFFE6 ||
        code in 0x1F300..0x1F64F ||
        code in 0x1F900..0x1F9FF ||
        code in 0x20000..0x3FFFD
}
