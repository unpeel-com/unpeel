package com.unpeel.android

import com.unpeel.controller.TerminalModeSignals
import com.unpeel.controller.VtScreen

/**
 * Remote-only terminal. Bytes come from the Host journal.
 * Display VT is [VtScreen] (wide chars, 256/truecolor, alt buffer).
 */
class HostTerminal(
    private val replies: (ByteArray) -> Unit,
    private val onCopy: (String) -> Unit,
) {
    var onFrame: (() -> Unit)? = null

    var screen = VtScreen(80, 24)
        private set

    val columns: Int get() = screen.cols
    val rows: Int get() = screen.rows

    private var live = false
    private var appliedCanvas = CANVAS
    private val modes = TerminalModeSignals()

    val hostModeSnapshot: Boolean get() = modes.hostModeSnapshot
    val sawMouseOrAlternateDisable: Boolean get() = modes.sawMouseOrAlternateDisable
    val mouseTrackingActive: Boolean get() = modes.mouseTracking
    val alternateBufferActive: Boolean get() = modes.alternateScreen
    val applicationCursorKeys: Boolean get() = modes.applicationCursorKeys

    var parkedScroll: Int = 0

    fun prepare(columns: Int, rows: Int, notify: Boolean = true) {
        live = false
        modes.reset()
        screen = VtScreen(columns.coerceIn(2, 300), rows.coerceIn(2, 120))
        if (notify) onFrame?.invoke()
    }

    fun applyCanvas(argb: Int) {
        appliedCanvas = argb
        // Session chrome / provider hex → VT default background (and a readable FG).
        val bg = argb and 0xFFFFFF
        val fg = readableFg(bg)
        screen.setDefaults(fg, bg)
    }

    private fun readableFg(bg: Int): Int {
        val r = (bg shr 16) and 0xff
        val g = (bg shr 8) and 0xff
        val b = bg and 0xff
        val luma = 0.2126 * r + 0.7152 * g + 0.0722 * b
        return if (luma < 140) 0xD0D0D0 else 0x1A1B1D
    }

    fun canvasColor(): Int = appliedCanvas

    fun resizeGrid(columns: Int, rows: Int) {
        val cols = columns.coerceIn(2, 300)
        val lines = rows.coerceIn(2, 120)
        if (cols == screen.cols && lines == screen.rows) return
        screen.resize(cols, lines)
        onFrame?.invoke()
    }

    fun feed(bytes: ByteArray, live: Boolean, notify: Boolean = true, hostSnapshot: Boolean = false) {
        if (bytes.isEmpty()) return
        if (hostSnapshot) modes.markHostSnapshot()
        modes.feed(bytes)
        this.live = live
        screen.feed(bytes)
        if (notify) onFrame?.invoke()
    }

    fun updateCellSize(width: Int, height: Int) = Unit

    fun visibleText(): String =
        screen.snapshot().joinToString("\n") { row ->
            row.joinToString("") { cell ->
                if (cell.ch == '\u0000') "" else cell.ch.toString()
            }.trimEnd()
        }

    fun selectedText(c1: Int, r1: Int, c2: Int, r2: Int, scrollOffset: Int): String {
        val frame = frame(scrollOffset)
        val top = minOf(r1, r2)
        val bottom = maxOf(r1, r2)
        val sb = StringBuilder()
        for (r in top..bottom) {
            val left = if (r == top) minOf(c1, c2) else 0
            val right = if (r == bottom) maxOf(c1, c2) else frame.cols - 1
            for (c in left..right.coerceAtLeast(left)) {
                val ch = frame.cell(r, c).ch
                if (ch != '\u0000') sb.append(ch)
            }
            if (r < bottom) sb.append('\n')
        }
        return sb.toString().trimEnd()
    }

    /** One paint frame: scrollback + live grid. Avoid per-cell snapshot(). */
    fun frame(scrollOffset: Int): TerminalFrame {
        val history = screen.scrollback()
        val grid = screen.snapshot()
        val (cr, cc) = screen.cursor()
        return TerminalFrame(history, grid, scrollOffset.coerceIn(0, history.size), cr, cc)
    }

    fun scrollbackLines(): Int = screen.scrollback().size

    fun cursor(): Pair<Int, Int> = screen.cursor()

    fun replyFromEmulator(data: ByteArray) {
        if (!live || data.isEmpty()) return
        replies(data)
    }

    fun copyText(text: String) {
        if (text.isNotEmpty()) onCopy(text)
    }

    companion object {
        const val CANVAS = 0xFF1A1B1D.toInt()
    }
}

data class TerminalFrame(
    val history: List<List<VtScreen.Cell>>,
    val grid: List<List<VtScreen.Cell>>,
    val scrollOffset: Int,
    val cursorRow: Int,
    val cursorCol: Int,
) {
    val rows: Int get() = grid.size
    val cols: Int get() = grid.firstOrNull()?.size ?: 0

    fun cell(rowFromTop: Int, col: Int): VtScreen.Cell {
        val absolute = history.size - scrollOffset + rowFromTop
        return when {
            absolute < history.size -> {
                val line = history[absolute]
                if (col in line.indices) line[col]
                else VtScreen.Cell(' ', VtScreen.DEFAULT_FG, VtScreen.DEFAULT_BG, false, false)
            }
            else -> {
                val gr = absolute - history.size
                if (gr in grid.indices && col in grid[gr].indices) grid[gr][col]
                else VtScreen.Cell(' ', VtScreen.DEFAULT_FG, VtScreen.DEFAULT_BG, false, false)
            }
        }
    }
}
