package com.termit.ui

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import com.termit.vt.Cell
import com.termit.vt.CellStyle
import com.termit.vt.ScreenBuffer
import com.termit.vt.VtEmulator
import com.termit.vt.WcWidth

/**
 * Renders a [VtEmulator] grid into a [Canvas].
 * Geometry matches Termux [TerminalRenderer]. Same-style runs are batched
 * into one drawText (Termux drawTextRun) — per-cell draw was the frame killer.
 */
class TerminalRenderer(
    textSize: Int,
    typeface: Typeface,
) {
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        this.typeface = typeface
        this.textSize = textSize.toFloat()
        // Keep →↑ in JetBrains Mono; never promote them to Noto Color Emoji.
        runCatching {
            val m = Paint::class.java.getMethod("setEmojiFallbackEnabled", Boolean::class.javaPrimitiveType)
            m.invoke(this, false)
        }
    }
    private val bgPaint = Paint()
    private val cursorPaint = Paint().apply { style = Paint.Style.FILL }

    /** Reused run buffer — avoids per-frame StringBuilder churn. */
    private val runChars = CharArray(512)
    private val scratch = CharArray(1)
    private var runLen = 0

    // Cell slightly wider than the glyph advance so Latin/CJK are not jammed;
    // do NOT use Paint.letterSpacing — it desyncs monospace runs and clipped
    // the arms off "K" when a wide char followed.
    private val glyphAdvance: Float = textPaint.measureText("X").coerceAtLeast(1f)
    val fontWidth: Float = glyphAdvance * 1.08f
    val fontLineSpacing: Int = kotlin.math.ceil(textPaint.fontSpacing.toDouble()).toInt().coerceAtLeast(1)
    private val fontAscent: Int = kotlin.math.ceil(textPaint.ascent().toDouble()).toInt()
    val fontLineSpacingAndAscent: Int = fontLineSpacing + fontAscent

    /** Phone appearance for the GrokNight / GrokDay truecolor table. */
    private var phoneNight = true

    /** Grok sessions only. Other agents have no cell-color pair table. */
    private var mapGrokTruecolor = false

    fun render(
        emulator: VtEmulator,
        canvas: Canvas,
        topRow: Int,
        selectionY1: Int,
        selectionY2: Int,
        selectionX1: Int,
        selectionX2: Int,
        cursorVisible: Boolean,
        night: Boolean = true,
        mapGrokTruecolor: Boolean = false,
    ) {
        phoneNight = night
        this.mapGrokTruecolor = mapGrokTruecolor
        val cols = emulator.mColumns
        val rows = emulator.mRows
        val palette = emulator.colors
        val screen = emulator.screen
        var y = fontLineSpacingAndAscent.toFloat()

        for (r in 0 until rows) {
            y += fontLineSpacing
            val row = topRow + r
            val cellTop = y - fontLineSpacing
            val textY = y - fontLineSpacingAndAscent

            // Flush state for this row's run coalescing.
            var runCol = -1
            var runCells = 0
            var runFg = 0
            var runBg = 0
            var runBold = false
            var runUnderline = false
            var runSelected = false
            runLen = 0

            fun flushRun() {
                if (runCol < 0 || runLen == 0) {
                    runCol = -1
                    runLen = 0
                    runCells = 0
                    return
                }
                val left = runCol * fontWidth
                val right = left + runCells * fontWidth
                // Always fill — skipping default-bg left uncleared glyphs when a
                // prior frame / truecolor cell occupied the slot (Grok composer
                // ghosts until a longer rewrite or a full TUI repaint).
                bgPaint.color = runBg
                canvas.drawRect(left, cellTop, right, y, bgPaint)
                textPaint.color = runFg
                textPaint.isFakeBoldText = runBold
                textPaint.isUnderlineText = runUnderline
                // One draw for the whole run. Scale the monospace advance up to
                // the cell pitch so the next cell (often CJK) starts on a grid
                // boundary instead of painting over the previous glyph.
                val scale = fontWidth / glyphAdvance
                canvas.save()
                canvas.clipRect(left, cellTop, right, y)
                canvas.translate(left, 0f)
                canvas.scale(scale, 1f)
                canvas.drawText(runChars, 0, runLen, 0f, textY, textPaint)
                canvas.restore()
                runCol = -1
                runLen = 0
                runCells = 0
            }

            fun paintWideChar(
                ch: Char,
                column: Int,
                widthCells: Int,
                fg: Int,
                bg: Int,
                bold: Boolean,
                underline: Boolean,
                selected: Boolean,
                inverse: Boolean,
            ) {
                val left = column * fontWidth
                val slot = fontWidth * widthCells
                val right = left + slot
                bgPaint.color = bg
                canvas.drawRect(left, cellTop, right, y, bgPaint)
                textPaint.color = fg
                textPaint.isFakeBoldText = bold
                textPaint.isUnderlineText = underline
                scratch[0] = ch
                // Fit into 85% of the wcwidth slot and center. Clip so CJK ink
                // cannot erase Latin overhang in the previous cell.
                val measured = textPaint.measureText(scratch, 0, 1).coerceAtLeast(0.01f)
                val target = slot * 0.85f
                val scaleX = target / measured
                val pad = (slot - target) * 0.5f
                canvas.save()
                canvas.clipRect(left, cellTop, right, y)
                if (kotlin.math.abs(scaleX - 1f) < 0.02f && pad < 0.5f) {
                    canvas.drawText(scratch, 0, 1, left + pad, textY, textPaint)
                } else {
                    canvas.translate(left + pad, 0f)
                    canvas.scale(scaleX, 1f)
                    canvas.drawText(scratch, 0, 1, 0f, textY, textPaint)
                }
                canvas.restore()
            }

            var c = 0
            while (c < cols) {
                val cell = screen.cellAtTranscript(row, c)
                if (cell.char == '\u0000') {
                    flushRun()
                    // Wide-char trailer or wiped slot: fill so a previous glyph
                    // cannot survive when the head cell changed this frame.
                    val left = c * fontWidth
                    bgPaint.color = resolveBg(cell, palette)
                    canvas.drawRect(left, cellTop, left + fontWidth, y, bgPaint)
                    c++
                    continue
                }
                val cp = cell.char.code
                // Width from wcwidth — never infer from a leftover \u0000 trailer
                // (that used to stretch │ into 2 cells and jog the box border).
                val widthCells = WcWidth.width(cp).coerceIn(1, 2)
                val selected = selectionY1 >= 0 && row in selectionY1..selectionY2 &&
                    c >= (if (row == selectionY1) selectionX1 else 0) &&
                    c <= (if (row == selectionY2) selectionX2 else cols - 1)

                // Builtin glyphs break runs — flush then paint one cell.
                if (BuiltinGlyphs.handles(cp)) {
                    flushRun()
                    paintBuiltin(canvas, cell, palette, c, y, cellTop, widthCells, selected)
                    c += widthCells
                    continue
                }

                var fg = resolveFg(cell, palette)
                var bg = resolveBg(cell, palette)
                if (selected) {
                    bg = 0xFF3A5F8A.toInt()
                    fg = 0xFFFFFFFF.toInt()
                }
                val bold = cell.bold && !selected
                val underline = cell.underline && !selected

                // Wide (CJK) must not share a drawText run with ASCII — the CJK
                // fallback advance ≠ 2×fontWidth, which left holes after "："/":".
                if (widthCells > 1) {
                    flushRun()
                    paintWideChar(
                        cell.char, c, widthCells, fg, bg, bold, underline, selected, cell.inverse,
                    )
                    c += widthCells
                    continue
                }

                val canJoin = runCol >= 0 &&
                    fg == runFg && bg == runBg &&
                    bold == runBold && underline == runUnderline &&
                    selected == runSelected &&
                    runLen < runChars.size

                if (!canJoin) {
                    flushRun()
                    runCol = c
                    runFg = fg
                    runBg = bg
                    runBold = bold
                    runUnderline = underline
                    runSelected = selected
                }
                runChars[runLen++] = cell.char
                runCells += 1
                c += 1
            }
            flushRun()

            val showCursor = cursorVisible && emulator.shouldCursorBeVisible() &&
                row == emulator.cursorRow
            if (showCursor) {
                drawCursor(canvas, emulator, screen, palette, row, y, cellTop, cols)
            }
        }
    }

    private fun resolveFg(cell: Cell, palette: IntArray): Int {
        // Default ink follows the cell's real background. A light bubble the
        // CLI painted keeps that background and gets dark ink; an unpainted
        // dark cell keeps light ink. Explicit SGR foreground is left alone,
        // except a Grok session's GrokNight / GrokDay truecolor.
        if (!cell.inverse && cell.fg == CellStyle.DEFAULT_FG) {
            val bg = resolvePainted(cell.bg, palette)
            return AnsiColors.resolve(
                AnsiColors.contrastingDefaultForeground(bg),
                palette,
                dim = cell.dim,
            )
        }
        var fg = resolvePainted(cell.fg, palette, bold = cell.bold, dim = cell.dim)
        val bg = resolvePainted(cell.bg, palette)
        if (cell.inverse) fg = bg
        return fg
    }

    private fun resolveBg(cell: Cell, palette: IntArray): Int {
        var fg = resolvePainted(cell.fg, palette, bold = cell.bold, dim = cell.dim)
        var bg = resolvePainted(cell.bg, palette)
        if (cell.inverse) {
            val tmp = fg
            fg = bg
            bg = tmp
        }
        return bg
    }

    private fun resolvePainted(
        color: Int,
        palette: IntArray,
        bold: Boolean = false,
        dim: Boolean = false,
    ): Int {
        val source = if (mapGrokTruecolor && AnsiColors.isTruecolor(color)) {
            GrokTruecolor.map(color, phoneNight)
        } else {
            color
        }
        return AnsiColors.resolve(source, palette, bold, dim)
    }

    private fun paintBuiltin(
        canvas: Canvas,
        cell: Cell,
        palette: IntArray,
        column: Int,
        y: Float,
        cellTop: Float,
        widthCells: Int,
        selected: Boolean,
    ) {
        var fg = resolveFg(cell, palette)
        var bg = resolveBg(cell, palette)
        if (selected) {
            bg = 0xFF3A5F8A.toInt()
            fg = 0xFFFFFFFF.toInt()
        }
        val left = column * fontWidth
        val right = left + fontWidth * widthCells
        val cp = cell.char.code
        // Powerline wedges still paint their own triangle; everything else
        // fills the cell so stale ink cannot remain.
        if (cp !in 0xE0B0..0xE0BF) {
            bgPaint.color = bg
            canvas.drawRect(left, cellTop, right, y, bgPaint)
        }
        BuiltinGlyphs.draw(canvas, cp, left, cellTop, right - left, fontLineSpacing.toFloat(), fg, bg)
    }

    private fun drawCursor(
        canvas: Canvas,
        emulator: VtEmulator,
        screen: ScreenBuffer,
        palette: IntArray,
        row: Int,
        y: Float,
        cellTop: Float,
        cols: Int,
    ) {
        var col = emulator.cursorCol.coerceIn(0, cols - 1)
        if (col > 0 && screen.cellAtTranscript(row, col).char == '\u0000') col--
        val cell = screen.cellAtTranscript(row, col)
        val widthCells = WcWidth.width(cell.char.code).coerceIn(1, 2)
        val cellBg = resolveBg(cell, palette)
        cursorPaint.color = AnsiColors.contrastingDefaultForeground(cellBg)
        val left = col * fontWidth
        val cursorHeight = (fontLineSpacingAndAscent - fontAscent).toFloat()
        canvas.drawRect(left, y - cursorHeight, left + fontWidth * widthCells, y, cursorPaint)
        if (cell.char == '\u0000' || cell.char == ' ') return
        val cp = cell.char.code
        if (BuiltinGlyphs.handles(cp)) {
            // Invert fg/bg so the glyph stays readable on the cursor block.
            val inverted = Cell().also {
                it.copyFrom(cell)
                it.inverse = !cell.inverse
            }
            paintBuiltin(canvas, inverted, palette, col, y, cellTop, widthCells, false)
        } else {
            textPaint.color = cellBg
            textPaint.isFakeBoldText = cell.bold
            textPaint.isUnderlineText = false
            runChars[0] = cell.char
            val measured = textPaint.measureText(runChars, 0, 1).coerceAtLeast(0.01f)
            val slot = fontWidth * widthCells
            if (widthCells > 1) {
                val target = slot * 0.85f
                val scaleX = target / measured
                val pad = (slot - target) * 0.5f
                canvas.save()
                canvas.translate(left + pad, 0f)
                canvas.scale(scaleX, 1f)
                canvas.drawText(runChars, 0, 1, 0f, y - fontLineSpacingAndAscent, textPaint)
                canvas.restore()
            } else {
                canvas.drawText(runChars, 0, 1, left, y - fontLineSpacingAndAscent, textPaint)
            }
        }
    }
}
