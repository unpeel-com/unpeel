package com.termit.vt

/**
 * Circular transcript + live screen, Termux-shaped.
 * Full-screen [scrollUp] is O(cols); region / insert-delete line ops copy within margins.
 */
class ScreenBuffer(
    cols: Int,
    rows: Int,
    scrollbackLimit: Int = 2000,
) {
    var cols: Int = cols.coerceAtLeast(1)
        private set
    var rows: Int = rows.coerceAtLeast(1)
        private set

    private val scrollbackLimit = scrollbackLimit.coerceAtLeast(0)
    private var totalRows: Int = (this.rows + this.scrollbackLimit).coerceAtLeast(this.rows)
    private var lines: Array<Array<Cell>> = Array(totalRows) { freshRow(this.cols) }

    /** Index of the top visible screen row inside [lines]. */
    private var screenFirst = 0

    /** How many rows above the screen are readable scrollback. */
    private var activeTranscript = 0

    /** DECSTBM inclusive margins (external screen rows). */
    var scrollTop: Int = 0
        private set
    var scrollBottom: Int = this.rows - 1
        private set

    val scrollbackLines: Int get() = activeTranscript

    /** Termux-shaped alias used by the Unpeel HostTerminalView scroll math. */
    val activeTranscriptRows: Int get() = activeTranscript

    private fun internal(externalRow: Int): Int {
        var i = screenFirst + externalRow
        val n = totalRows
        i %= n
        if (i < 0) i += n
        return i
    }

    fun cell(row: Int, col: Int): Cell = lines[internal(row)][col]

    /**
     * Row 0 is the top of the live screen; negative rows walk into scrollback
     * (same convention as Termux getSelectedText / render topRow).
     */
    fun cellAtTranscript(row: Int, col: Int): Cell {
        if (col !in 0 until cols) return BLANK
        if (row < -activeTranscript || row >= rows) return BLANK
        return lines[internal(row)][col]
    }

    fun visibleCell(rowFromTop: Int, col: Int, scrollOffset: Int): Cell {
        val absolute = activeTranscript - scrollOffset + rowFromTop
        val external = absolute - activeTranscript
        return cellAtTranscript(external, col)
    }

    fun setScrollRegion(top: Int, bottom: Int) {
        scrollTop = top.coerceIn(0, rows - 1)
        scrollBottom = bottom.coerceIn(scrollTop, rows - 1)
    }

    fun resetScrollRegion() {
        scrollTop = 0
        scrollBottom = rows - 1
    }

    fun resize(newCols: Int, newRows: Int) {
        val c = newCols.coerceAtLeast(1)
        val r = newRows.coerceAtLeast(1)
        if (c == cols && r == rows) return
        // Phone letterbox often changes rows only. Dropping transcript here
        // made Cursor local flick lose everything right after attach/fit.
        if (c == cols) {
            resizeRowsOnly(r)
            return
        }
        // Column change rewraps; caller rebuilds from a Host snapshot.
        val nextTotal = (r + scrollbackLimit).coerceAtLeast(r)
        val next = Array(nextTotal) { freshRow(c) }
        val copyRows = minOf(rows, r)
        val copyCols = minOf(cols, c)
        for (y in 0 until copyRows) {
            val src = lines[internal(y)]
            val dst = next[y]
            for (x in 0 until copyCols) dst[x].copyFrom(src[x])
        }
        cols = c
        rows = r
        totalRows = nextTotal
        lines = next
        screenFirst = 0
        activeTranscript = 0
        resetScrollRegion()
    }

    /**
     * Grow/shrink height without discarding scrollback. Shrinking moves the
     * lines that fall off the bottom of the screen into transcript (xterm-ish).
     */
    private fun resizeRowsOnly(newRows: Int) {
        val r = newRows.coerceAtLeast(1)
        if (r == rows) return
        val oldRows = rows
        val oldTranscript = activeTranscript
        val depth = oldTranscript + oldRows
        val ordered = Array(depth) { freshRow(cols) }
        for (i in 0 until oldTranscript) {
            copyFullRow(lines[internal(-oldTranscript + i)], ordered[i])
        }
        for (y in 0 until oldRows) {
            copyFullRow(lines[internal(y)], ordered[oldTranscript + y])
        }

        val kept = ArrayList<Array<Cell>>(depth + (r - oldRows).coerceAtLeast(0))
        if (r >= oldRows) {
            for (i in 0 until depth) kept.add(ordered[i])
            repeat(r - oldRows) { kept.add(freshRow(cols)) }
        } else {
            for (i in 0 until oldTranscript) kept.add(ordered[i])
            for (y in r until oldRows) kept.add(ordered[oldTranscript + y])
            while (kept.size > scrollbackLimit) kept.removeAt(0)
            for (y in 0 until r) kept.add(ordered[oldTranscript + y])
        }

        val nextTotal = (r + scrollbackLimit).coerceAtLeast(r)
        val next = Array(nextTotal) { freshRow(cols) }
        val screenStart = kept.size - r
        val newTranscript = screenStart.coerceIn(0, scrollbackLimit)
        for (y in 0 until r) {
            copyFullRow(kept[screenStart + y], next[y])
        }
        for (i in 0 until newTranscript) {
            copyFullRow(kept[screenStart - newTranscript + i], next[nextTotal - newTranscript + i])
        }
        rows = r
        totalRows = nextTotal
        lines = next
        screenFirst = 0
        activeTranscript = newTranscript
        resetScrollRegion()
    }

    private fun copyFullRow(from: Array<Cell>, to: Array<Cell>) {
        val n = minOf(from.size, to.size)
        for (x in 0 until n) to[x].copyFrom(from[x])
    }

    fun clearAll() {
        for (y in 0 until rows) clearRow(internal(y))
    }

    /** CSI 3J — wipe scrollback history. */
    fun clearScrollback() {
        activeTranscript = 0
    }

    fun clearFrom(row: Int, col: Int) {
        val line = lines[internal(row)]
        for (x in col until cols) line[x].reset()
        for (y in row + 1 until rows) clearRow(internal(y))
    }

    /** CSI J mode 1 — clear from origin through cursor (inclusive). */
    fun clearAbove(row: Int, col: Int) {
        for (y in 0 until row) clearRow(internal(y))
        val line = lines[internal(row)]
        for (x in 0..col.coerceIn(0, cols - 1)) line[x].reset()
    }

    fun clearLineFrom(row: Int, col: Int) {
        val line = lines[internal(row)]
        for (x in col until cols) line[x].reset()
    }

    fun clearLineTo(row: Int, col: Int) {
        val line = lines[internal(row)]
        for (x in 0..col.coerceIn(0, cols - 1)) line[x].reset()
    }

    fun clearLine(row: Int) {
        clearRow(internal(row))
    }

    /** Push top of region; blank bottom of region. Full screen uses the ring. */
    fun scrollUp() {
        if (scrollTop == 0 && scrollBottom == rows - 1) {
            scrollUpFull()
            return
        }
        shiftRegionUp(scrollTop, scrollBottom)
    }

    /** Insert blank at top of region; drop bottom. */
    fun scrollDown() {
        if (scrollTop == 0 && scrollBottom == rows - 1) {
            scrollDownFull()
            return
        }
        shiftRegionDown(scrollTop, scrollBottom)
    }

    /** CSI L — insert [count] blank lines at [atRow], pushing down within the scroll region. */
    fun insertLines(atRow: Int, count: Int) {
        if (atRow !in scrollTop..scrollBottom) return
        val n = count.coerceAtLeast(1).coerceAtMost(scrollBottom - atRow + 1)
        repeat(n) { shiftRegionDown(atRow, scrollBottom) }
    }

    /** CSI M — delete [count] lines at [atRow], pulling up within the scroll region. */
    fun deleteLines(atRow: Int, count: Int) {
        if (atRow !in scrollTop..scrollBottom) return
        val n = count.coerceAtLeast(1).coerceAtMost(scrollBottom - atRow + 1)
        repeat(n) { shiftRegionUp(atRow, scrollBottom) }
    }

    /** CSI @ — insert blank cells at (row, col). */
    fun insertChars(row: Int, col: Int, count: Int) {
        if (row !in 0 until rows || col !in 0 until cols) return
        val n = count.coerceAtLeast(1)
        val line = lines[internal(row)]
        for (x in cols - 1 downTo col + n) {
            if (x - n >= col) line[x].copyFrom(line[x - n])
        }
        for (x in col until (col + n).coerceAtMost(cols)) line[x].reset()
    }

    /** CSI P — delete cells at (row, col), shifting left. */
    fun deleteChars(row: Int, col: Int, count: Int) {
        if (row !in 0 until rows || col !in 0 until cols) return
        val n = count.coerceAtLeast(1)
        val line = lines[internal(row)]
        var x = col
        while (x + n < cols) {
            line[x].copyFrom(line[x + n])
            x++
        }
        while (x < cols) {
            line[x].reset()
            x++
        }
    }

    /** CSI X — erase [count] cells from (row, col) without shifting. */
    fun eraseChars(row: Int, col: Int, count: Int) {
        if (row !in 0 until rows || col !in 0 until cols) return
        val line = lines[internal(row)]
        val end = (col + count.coerceAtLeast(1)).coerceAtMost(cols)
        for (x in col until end) line[x].reset()
    }

    private fun scrollUpFull() {
        if (scrollbackLimit > 0) {
            if (activeTranscript < scrollbackLimit) activeTranscript++
        }
        screenFirst = (screenFirst + 1) % totalRows
        clearRow(internal(rows - 1))
    }

    private fun scrollDownFull() {
        screenFirst = (screenFirst - 1 + totalRows) % totalRows
        if (activeTranscript > 0) activeTranscript--
        clearRow(internal(0))
    }

    private fun shiftRegionUp(top: Int, bottom: Int) {
        // DECSTBM scroll: keep the line leaving the top of the region in
        // scrollback so Cursor-style TUIs stay flickable on the phone.
        if (scrollbackLimit > 0 && top == scrollTop) {
            pushExternalRowToScrollback(top)
        }
        for (y in top until bottom) {
            copyExternalRow(y + 1, y)
        }
        clearRow(internal(bottom))
    }

    /**
     * Copy [externalRow] into scrollback. Does not move the live screen
     * window — [shiftRegionUp] still slides the region afterward.
     */
    private fun pushExternalRowToScrollback(externalRow: Int) {
        if (scrollbackLimit <= 0) return
        val dest = if (activeTranscript < scrollbackLimit) {
            internal(-activeTranscript - 1)
        } else {
            // Ring full: overwrite the oldest history row.
            internal(-activeTranscript)
        }
        val src = lines[internal(externalRow)]
        for (x in 0 until cols) lines[dest][x].copyFrom(src[x])
        if (activeTranscript < scrollbackLimit) activeTranscript++
    }

    private fun shiftRegionDown(top: Int, bottom: Int) {
        for (y in bottom downTo top + 1) {
            copyExternalRow(y - 1, y)
        }
        clearRow(internal(top))
    }

    private fun copyExternalRow(from: Int, to: Int) {
        val src = lines[internal(from)]
        val dst = lines[internal(to)]
        for (x in 0 until cols) dst[x].copyFrom(src[x])
    }

    private fun clearRow(internalRow: Int) {
        val line = lines[internalRow]
        for (x in 0 until cols) line[x].reset()
    }

    private fun freshRow(c: Int): Array<Cell> = Array(c) { Cell() }

    companion object {
        private val BLANK = Cell()
    }
}
