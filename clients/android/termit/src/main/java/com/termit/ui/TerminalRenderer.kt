package com.termit.ui

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import com.termit.vt.Cell
import com.termit.vt.CellStyle
import com.termit.vt.VtEmulator

/**
 * Renders a [VtEmulator] grid into a [Canvas].
 * Geometry matches Termux [TerminalRenderer]: row cursor `y` grows by
 * lineSpacing each row; glyph baseline is `y - fontLineSpacingAndAscent`;
 * non-default backgrounds fill `y - lineSpacing .. y`.
 */
class TerminalRenderer(
    textSize: Int,
    typeface: Typeface,
) {
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        this.typeface = typeface
        this.textSize = textSize.toFloat()
    }
    private val fallbackPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        this.typeface = Typeface.DEFAULT
        this.textSize = textSize.toFloat()
    }
    private val bgPaint = Paint()
    private val cursorPaint = Paint().apply { style = Paint.Style.FILL }
    private val inkBounds = Rect()

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
            if (cursorVisible && row == emulator.cursorRow && emulator.cursorCol in 0 until cols) {
                cursorPaint.color = AnsiColors.resolve(AnsiColors.INDEX_CURSOR, palette)
                val left = emulator.cursorCol * fontWidth
                val cursorHeight = (fontLineSpacingAndAscent - fontAscent).toFloat()
                canvas.drawRect(left, y - cursorHeight, left + fontWidth, y, cursorPaint)
                val cell = emulator.screen.cellAtTranscript(row, emulator.cursorCol)
                if (cell.char != '\u0000') {
                    drawRun(canvas, cell, palette, emulator.cursorCol, y, 1, selected = false, forceFg = true)
                }
            }
        }
    }

    private fun drawRun(
        canvas: Canvas,
        cell: Cell,
        palette: IntArray,
        column: Int,
        y: Float,
        widthCells: Int,
        selected: Boolean,
        forceFg: Boolean = false,
    ) {
        var fg = AnsiColors.resolve(cell.fg, palette, bold = cell.bold, dim = cell.dim)
        var bg = AnsiColors.resolve(cell.bg, palette)
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
        val right = left + fontWidth * widthCells
        val defaultBg = AnsiColors.resolve(AnsiColors.INDEX_BG, palette)
        if (selected || cell.inverse || bg != defaultBg) {
            // Termux: only non-default backgrounds; rect is [y - lineSpacing, y].
            bgPaint.color = bg
            canvas.drawRect(left, y - fontLineSpacing, right, y, bgPaint)
        }

        val ch = cell.char
        if (!forceFg && (ch == ' ' || ch == '\u0000')) return

        textPaint.color = fg
        textPaint.isFakeBoldText = cell.bold
        textPaint.isUnderlineText = cell.underline
        val paint = textPaint
        val glyph = ch.toString()
        paint.getTextBounds(glyph, 0, glyph.length, inkBounds)
        val inkWidth = (inkBounds.right - inkBounds.left).toFloat().coerceAtLeast(1f)
        val cellW = right - left
        var drawX = left
        val textY = y - fontLineSpacingAndAscent
        var saved = false
        if (inkWidth > 1f && (inkBounds.left < -0.5f || inkBounds.right > cellW + 0.5f)) {
            val scale = minOf(1f, cellW / inkWidth)
            drawX = left + (cellW - inkBounds.left - inkBounds.right) * 0.5f
            if (scale < 0.999f) {
                val cx = left + cellW * 0.5f
                val cy = y - fontLineSpacing * 0.5f
                canvas.save()
                canvas.translate(cx, cy)
                canvas.scale(scale, scale)
                canvas.translate(-cx, -cy)
                saved = true
            }
        }
        canvas.drawText(glyph, drawX, textY, paint)
        if (saved) canvas.restore()
    }
}
