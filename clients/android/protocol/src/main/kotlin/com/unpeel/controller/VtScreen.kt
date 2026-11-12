package com.unpeel.controller

/**
 * A display-only VT for a Host-owned PTY. The phone may POST
 * `/mobile/resize-desktop` once, after the terminal viewport is measured,
 * so the Host letterboxes to that grid. Callers then size this screen to
 * the confirmed columns and rows. The keyboard does not resize the PTY.
 */
class VtScreen(cols: Int, rows: Int) {
    var cols = cols.coerceIn(2, 300)
        private set
    var rows = rows.coerceIn(2, 200)
        private set

    data class Cell(
        val ch: Char,
        val fg: Int,
        val bg: Int,
        val bold: Boolean,
        val dim: Boolean = false,
    )

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

    var defaultFg: Int = DEFAULT_FG
        private set
    var defaultBg: Int = DEFAULT_BG
        private set

    private var fg = defaultFg
    private var bg = defaultBg
    private var bold = false
    private var dim = false
    private var utf8 = ByteArray(0)

    private enum class Mode { GROUND, ESC, CSI, OSC }

    private var mode = Mode.GROUND
    private val csi = StringBuilder()
    private var csiPrivate = false
    private val osc = StringBuilder()

    fun resize(columns: Int, lines: Int) {
        val nextCols = columns.coerceIn(2, 300)
        val nextRows = lines.coerceIn(2, 200)
        if (nextCols == cols && nextRows == rows) return
        val keepHistory = nextCols == cols
        val preserved = if (keepHistory) ArrayDeque(history) else ArrayDeque()
        // Row-only refit (phone chrome / keyboard) must not throw away the
        // transcript — that is what Mac Ghostty keeps across a resize.
        // A column change reflows wrapping, so history cannot be reused.
        cols = nextCols
        rows = nextRows
        normal = Grid(cols, rows)
        alt = Grid(cols, rows)
        onAlt = false
        history.clear()
        history.addAll(preserved)
        while (history.size > SCROLLBACK) history.removeFirst()
        resetStyle()
        mode = Mode.GROUND
        utf8 = ByteArray(0)
    }

    fun reset() {
        resize(cols, rows)
    }

    /** Session / OSC theme defaults. Affects SGR 39/49 and freshly cleared cells. */
    fun setDefaults(foreground: Int, background: Int) {
        defaultFg = foreground and 0xFFFFFF
        defaultBg = background and 0xFFFFFF
        if (fg == DEFAULT_FG || fg == defaultFg) fg = defaultFg
        if (bg == DEFAULT_BG || bg == defaultBg) bg = defaultBg
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
                    ']'.code -> {
                        osc.clear()
                        mode = Mode.OSC
                    }
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
            Mode.OSC -> when (code) {
                0x07 -> {
                    applyOsc(osc.toString())
                    osc.clear()
                    mode = Mode.GROUND
                }
                0x1B -> {
                    applyOsc(osc.toString())
                    osc.clear()
                    mode = Mode.ESC
                }
                else -> if (osc.length < 512) osc.append(code.toChar())
            }
        }
    }

    private fun applyOsc(body: String) {
        val split = body.indexOf(';')
        val ps = if (split < 0) body else body.substring(0, split)
        val pt = if (split < 0) "" else body.substring(split + 1)
        when (ps) {
            "10" -> parseOscColor(pt)?.let { setDefaults(it, defaultBg); fg = defaultFg }
            "11" -> parseOscColor(pt)?.let { setDefaults(defaultFg, it); bg = defaultBg }
            "12" -> Unit // cursor color — view draws its own caret
            "104", "110" -> {
                defaultFg = DEFAULT_FG
                fg = defaultFg
            }
            "111" -> {
                defaultBg = DEFAULT_BG
                bg = defaultBg
            }
        }
    }

    private fun parseOscColor(spec: String): Int? {
        val s = spec.trim().removePrefix("rgb:").removePrefix("#")
        return when {
            s.matches(Regex("^[0-9a-fA-F]{6}$")) -> s.toIntOrNull(16)
            s.matches(Regex("^[0-9a-fA-F]{3}$")) -> {
                val r = s[0].digitToInt(16) * 0x11
                val g = s[1].digitToInt(16) * 0x11
                val b = s[2].digitToInt(16) * 0x11
                (r shl 16) or (g shl 8) or b
            }
            // xterm rgb:RRRR/GGGG/BBBB
            s.contains('/') -> {
                val parts = s.split('/')
                if (parts.size != 3) return null
                fun chan(raw: String): Int {
                    val v = raw.toIntOrNull(16) ?: return 0
                    return when (raw.length) {
                        1, 2 -> v and 0xff
                        else -> (v shr 8) and 0xff
                    }
                }
                (chan(parts[0]) shl 16) or (chan(parts[1]) shl 8) or chan(parts[2])
            }
            else -> null
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
                2 -> dim = true
                22 -> {
                    bold = false
                    dim = false
                }
                39 -> fg = defaultFg
                49 -> bg = defaultBg
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
        grid.cells[grid.cursorRow][grid.cursorCol] = Cell(ch, fg, bg, bold, dim)
        if (width == 2 && grid.cursorCol + 1 < cols) {
            grid.cells[grid.cursorRow][grid.cursorCol + 1] = Cell('\u0000', fg, bg, false, dim)
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
        // Agent TUIs (Cursor/Claude) often scroll a primary-screen region that
        // does not start at row 0. Classic xterm would drop those lines; the
        // phone must keep them or local pan hits a tiny ceiling while Mac
        // Ghostty still has the transcript.
        if (!onAlt) {
            history.addLast(cells[grid.scrollTop].copyOf())
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
            if (col in 0 until cols) grid.cells[row][col] = Cell(' ', fg, bg, false, false)
        }
    }

    private fun blankRow() = Array(cols) { Cell(' ', defaultFg, defaultBg, false, false) }

    private fun resetStyle() {
        fg = defaultFg
        bg = defaultBg
        bold = false
        dim = false
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
        private const val SCROLLBACK = 20000
        private val ANSI = intArrayOf(
            0x0C0C0C, 0xCD3131, 0x0DBC79, 0xE5E510, 0x2472C8, 0xBC3FBC, 0x11A8CD, 0xCCCCCC,
        )
        private val BRIGHT = intArrayOf(
            0x766F6F, 0xF14C4C, 0x23D18B, 0xF5F543, 0x3B8EEA, 0xD670D6, 0x29B8DB, 0xFFFFFF,
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
