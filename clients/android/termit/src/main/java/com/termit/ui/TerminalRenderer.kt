package com.termit.ui

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import com.termit.vt.Cell
import com.termit.vt.VtEmulator

/**
 * Renders a [VtEmulator] grid into a [Canvas].
 * Wide glyphs must not be painted over by the next cell's background — that is
 * what made CJK look shredded when HostTerminalView drew cell-by-cell.
 */
class TerminalRenderer(
    textSize: Int,
    typeface: Typeface,
) {
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        this.typeface = typeface
        this.textSize = textSize.toFloat()
    }
    private val bgPaint = Paint()
    private val cursorPaint = Paint().apply {
        color = 0x99FFFFFF.toInt()
        style = Paint.Style.FILL
    }

    val fontWidth: Float = textPaint.measureText("X").coerceAtLeast(1f)
    val fontLineSpacing: Int = kotlin.math.ceil(textPaint.fontSpacing.toDouble()).toInt().coerceAtLeast(1)
    private val fontAscent: Int = kotlin.math.ceil(textPaint.ascent().toDouble()).toInt()
    val fontLineSpacingAndAscent: Int = fontLineSpacing + fontAscent

    /**
     * @param topRow transcript-style row of the first visible line (may be negative)
     * @param selectionY1..selectionX2 selection in emulator coords, or -1 for none
     */
    fun render(
        emulator: VtEmulator,
        canvas: Canvas,
        topRow: Int,
        selectionY1: Int,
        selectionY2: Int,
        selectionX1: Int,
        selectionX2: Int,
        cursorVisible: Boolean,
    ) {
        val cols = emulator.mColumns
        val rows = emulator.mRows
        var y = fontLineSpacingAndAscent.toFloat()
        for (r in 0 until rows) {
            val row = topRow + r
            var c = 0
            while (c < cols) {
                val cell = emulator.screen.cellAtTranscript(row, c)
                if (cell.char == '\u0000') {
                    // Wide-glyph continuation: never paint — would erase the previous glyph.
                    c++
                    continue
                }
                val widthCells = if (c + 1 < cols && emulator.screen.cellAtTranscript(row, c + 1).char == '\u0000') 2 else 1
                val selected = selectionY1 >= 0 && row in selectionY1..selectionY2 &&
                    c >= (if (row == selectionY1) selectionX1 else 0) &&
                    c <= (if (row == selectionY2) selectionX2 else cols - 1)
                drawRun(canvas, cell, c, y, widthCells, selected)
                c += widthCells
            }
            if (cursorVisible && row == emulator.cursorRow) {
                val left = emulator.cursorCol * fontWidth
                val top = y - fontLineSpacingAndAscent
                canvas.drawRect(left, top, left + fontWidth, top + fontLineSpacing, cursorPaint)
                if (emulator.cursorCol in 0 until cols) {
                    val cell = emulator.screen.cellAtTranscript(row, emulator.cursorCol)
                    if (cell.char != '\u0000') {
                        drawRun(canvas, cell, emulator.cursorCol, y, 1, selected = false, forceFg = true)
                    }
                }
            }
            y += fontLineSpacing
        }
    }

    private fun drawRun(
        canvas: Canvas,
        cell: Cell,
        column: Int,
        baselineY: Float,
        widthCells: Int,
        selected: Boolean,
        forceFg: Boolean = false,
    ) {
        var fg = AnsiColors.foreground(cell.fg, cell.bold)
        var bg = AnsiColors.background(cell.bg)
        if (cell.inverse) {
            val tmp = fg
            fg = bg
            bg = tmp
        }
        if (selected) {
            bg = 0xFF3A5F8A.toInt()
            fg = 0xFFFFFFFF.toInt()
        }
        val left = column * fontWidth
        val top = baselineY - fontLineSpacingAndAscent
        val right = left + fontWidth * widthCells
        bgPaint.color = bg
        canvas.drawRect(left, top, right, top + fontLineSpacing, bgPaint)
        val ch = cell.char
        if (forceFg || (ch != ' ' && ch != '\u0000')) {
            textPaint.color = fg
            textPaint.isFakeBoldText = cell.bold
            textPaint.isUnderlineText = cell.underline
            // Scale down only when the glyph is wider than its cells (never scale CJK up).
            val expected = fontWidth * widthCells
            val measured = textPaint.measureText(ch.toString())
            canvas.save()
            if (measured > expected + 0.5f) {
                canvas.scale(expected / measured, 1f, left, baselineY)
            }
            canvas.drawText(ch.toString(), left, baselineY, textPaint)
            canvas.restore()
        }
    }
}
