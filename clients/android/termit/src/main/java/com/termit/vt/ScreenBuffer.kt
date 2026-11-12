package com.termit.vt

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
    private val scrollback = ArrayDeque<Array<Cell>>()
    private var grid: Array<Array<Cell>> = freshGrid(this.cols, this.rows)

    val scrollbackLines: Int get() = scrollback.size

    /** Termux-shaped alias used by the Unpeel HostTerminalView scroll math. */
    val activeTranscriptRows: Int get() = scrollback.size

    fun cell(row: Int, col: Int): Cell = grid[row][col]

    /**
     * Row 0 is the top of the live screen; negative rows walk into scrollback
     * (same convention as Termux getSelectedText / render topRow).
     */
    fun cellAtTranscript(row: Int, col: Int): Cell {
        if (col !in 0 until cols) return Cell()
        return when {
            row < 0 -> {
                val idx = scrollback.size + row
                if (idx !in scrollback.indices) return Cell()
                val line = scrollback.elementAt(idx)
                if (col < line.size) line[col] else Cell()
            }
            row in 0 until rows -> grid[row][col]
            else -> Cell()
        }
    }

    fun visibleCell(rowFromTop: Int, col: Int, scrollOffset: Int): Cell {
        val absolute = scrollback.size - scrollOffset + rowFromTop
        return when {
            absolute < scrollback.size -> {
                val line = scrollback.elementAt(absolute)
                if (col < line.size) line[col] else Cell()
            }
            else -> {
                val r = absolute - scrollback.size
                if (r in 0 until rows && col in 0 until cols) grid[r][col] else Cell()
            }
        }
    }

    fun resize(newCols: Int, newRows: Int) {
        val c = newCols.coerceAtLeast(1)
        val r = newRows.coerceAtLeast(1)
        if (c == cols && r == rows) return
        val next = freshGrid(c, r)
        val copyRows = minOf(rows, r)
        val copyCols = minOf(cols, c)
        for (y in 0 until copyRows) {
            for (x in 0 until copyCols) {
                next[y][x].copyFrom(grid[y][x])
            }
        }
        cols = c
        rows = r
        grid = next
    }

    fun clearAll() {
        for (y in 0 until rows) {
            for (x in 0 until cols) grid[y][x].reset()
        }
    }

    fun clearFrom(row: Int, col: Int) {
        for (x in col until cols) grid[row][x].reset()
        for (y in row + 1 until rows) {
            for (x in 0 until cols) grid[y][x].reset()
        }
    }

    fun clearLineFrom(row: Int, col: Int) {
        for (x in col until cols) grid[row][x].reset()
    }

    fun scrollUp() {
        if (scrollbackLimit > 0) {
            val saved = Array(cols) { Cell() }
            for (x in 0 until cols) saved[x].copyFrom(grid[0][x])
            scrollback.addLast(saved)
            while (scrollback.size > scrollbackLimit) scrollback.removeFirst()
        }
        for (y in 0 until rows - 1) {
            for (x in 0 until cols) grid[y][x].copyFrom(grid[y + 1][x])
        }
        for (x in 0 until cols) grid[rows - 1][x].reset()
    }

    private fun freshGrid(c: Int, r: Int): Array<Array<Cell>> =
        Array(r) { Array(c) { Cell() } }
}
