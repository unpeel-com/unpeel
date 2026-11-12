package com.termit.ui

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import com.termit.vt.Cell
import com.termit.vt.CellStyle
import com.termit.vt.VtEmulator

/**
 * Renders a [VtEmulator] grid into a [Canvas].
 * Row baselines match Termux TerminalRenderer (first baseline = ascentPad + lineSpacing).
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
        style = Paint.Style.FILL
    }

    val fontWidth: Float = textPaint.measureText("X").coerceAtLeast(1f)
    val fontLineSpacing: Int = kotlin.math.ceil(textPaint.fontSpacing.toDouble()).toInt().coerceAtLeast(1)
    private val fontAscent: Int = kotlin.math.ceil(textPaint.ascent().toDouble()).toInt()
    val fontLineSpacingAndAscent: Int = fontLineSpacing + fontAscent

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
        val palette = emulator.colors
        // Termux: heightOffset starts at ascentPad, then += lineSpacing before each row.
        var y = fontLineSpacingAndAscent.toFloat()
        for (r in 0 until rows) {
            y += fontLineSpacing
            val row = topRow + r
            var c = 0
            while (c < cols) {
                val cell = emulator.screen.cellAtTranscript(row, c)
                if (cell.char == '\u0000') {
                    c++
                    continue
                }
                val widthCells =
                    if (c + 1 < cols && emulator.screen.cellAtTranscript(row, c + 1).char == '\u0000') 2 else 1
                val selected = selectionY1 >= 0 && row in selectionY1..selectionY2 &&
                    c >= (if (row == selectionY1) selectionX1 else 0) &&
                    c <= (if (row == selectionY2) selectionX2 else cols - 1)
                drawRun(canvas, cell, palette, c, y, widthCells, selected)
                c += widthCells
            }
            if (cursorVisible && row == emulator.cursorRow) {
                cursorPaint.color = AnsiColors.resolve(AnsiColors.INDEX_CURSOR, palette)
                val left = emulator.cursorCol * fontWidth
                val top = y - fontLineSpacingAndAscent
                canvas.drawRect(left, top, left + fontWidth, top + fontLineSpacing, cursorPaint)
                if (emulator.cursorCol in 0 until cols) {
                    val cell = emulator.screen.cellAtTranscript(row, emulator.cursorCol)
                    if (cell.char != '\u0000') {
                        drawRun(canvas, cell, palette, emulator.cursorCol, y, 1, selected = false, forceFg = true)
                    }
                }
            }
        }
    }

    private fun drawRun(
        canvas: Canvas,
        cell: Cell,
        palette: IntArray,
        column: Int,
        baselineY: Float,
        widthCells: Int,
        selected: Boolean,
        forceFg: Boolean = false,
    ) {
        var fg = AnsiColors.resolve(cell.fg, palette, bold = cell.bold, dim = cell.dim)
        var bg = AnsiColors.resolve(cell.bg, palette, bold = false, dim = false)
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
        // Default background: skip fill so the view's canvasColor shows through
        // (matches Termux painting default bg from the palette that Host sets to CANVAS).
        val isDefaultBg = !selected && !cell.inverse && cell.bg == CellStyle.DEFAULT_BG
        if (!isDefaultBg) {
            bgPaint.color = bg
            canvas.drawRect(left, top, right, top + fontLineSpacing, bgPaint)
        }
        val ch = cell.char
        if (forceFg || (ch != ' ' && ch != '\u0000')) {
            textPaint.color = fg
            textPaint.isFakeBoldText = cell.bold
            textPaint.isUnderlineText = cell.underline
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
