package com.unpeel.android

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Typeface
import android.text.InputType
import android.util.SparseIntArray
import android.util.TypedValue
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import com.termit.ui.TerminalKeys
import com.unpeel.controller.AgentTerminalPolicy
import com.unpeel.controller.VtScreen
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * Draws a Host terminal with a fixed glyph grid. Wide (CJK) cells occupy two
 * columns; continuation cells are `\u0000` and are not painted again.
 */
@SuppressLint("ViewConstructor", "ClickableViewAccessibility")
class HostTerminalView(
    context: Context,
    private var terminal: HostTerminal,
) : View(context) {
    private val baseTextSize =
        TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 13f, resources.displayMetrics)
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Typeface.MONOSPACE
        textSize = baseTextSize
        // Prefer text glyphs over Noto Color Emoji when the platform exposes
        // the switch (hidden on some SDK stubs — reflect so compile stays clean).
        disableEmojiFallback(this)
    }
    private val bgPaint = Paint()
    private val cursorPaint = Paint().apply {
        color = 0x99FFFFFF.toInt()
        style = Paint.Style.FILL
    }
    private val selectionPaint = Paint().apply {
        color = 0x553D7EFF
        style = Paint.Style.FILL
    }
    private val shapePath = Path()
    /** 0 unknown, 1 mono has glyph, 2 missing — same cache as stock Android terminal. */
    private val glyphCache = SparseIntArray(64)

    val cellWidth: Float
    val cellHeight: Int
    val topPad: Int
    private val baseline: Float

    var onSelectionChanged: ((Boolean) -> Unit)? = null
    var onMenuChanged: ((Boolean) -> Unit)? = null
    var prefersRemoteWheel: Boolean = false
    /** cursor-agent: neither SGR nor ↑/↓ — local Ghostty-style scrollback. */
    var echoesMouseWheel: Boolean = false
    var relayed: Boolean = false
    var canvasColor: Int = HostTerminal.CANVAS
        set(value) {
            if (field == value) return
            field = value
            setBackgroundColor(value)
            invalidate()
        }
    var onText: ((String) -> Unit)? = null
    var controlArmed: () -> Boolean = { false }
    var onControlUsed: () -> Unit = {}

    private var offsetRows = 0
    private var scrollRemainder = 0f
    private var wheelRemainder = 0f
    private var selecting = false
    private var selC1 = -1
    private var selR1 = -1
    private var selC2 = -1
    private var selR2 = -1
    private var cursorVisible = true
    private var menuActive = false
    /** Painted cell size — uniform scale so glyphs stay square; letterbox if needed. */
    private var drawCellW = 0f
    private var drawCellH = 0f
    private var drawOriginX = 0f
    private var drawOriginY = 0f
    private var drawBaseline = 0f

    private val frameListener: () -> Unit = {
        clampOffset()
        publishMenu()
        postInvalidateOnAnimation()
    }

    private val blink = object : Runnable {
        override fun run() {
            cursorVisible = !cursorVisible
            invalidate()
            postDelayed(this, 530L)
        }
    }

    private val gesture = GestureDetector(context, object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent): Boolean = true

        override fun onSingleTapUp(e: MotionEvent): Boolean {
            if (selecting) {
                clearSelection()
                return true
            }
            // Claude's virtual-scroll chip: "Jump to bottom (click)" or
            // "(ctrl+End)". iOS fakes ctrl+End via Ghostty; we send a click
            // when the chip asks for one, else CSI ctrl+End. Always clear
            // local scrollback so the phone matches.
            if (handleTuiJumpTap(e)) return true
            // Stock Unpeel Android / iOS: only click when the program asked
            // for mouse reports (Stop, Run Everything, menus, links).
            if (!terminal.mouseTrackingActive) {
                showKeyboard()
                return true
            }
            sendClick(e)
            if (tapTargetsCursor(e)) showKeyboard() else dismissKeyboard()
            return true
        }

        override fun onLongPress(e: MotionEvent) {
            val cell = cellAt(e.x, e.y) ?: return
            selecting = true
            selC1 = cell.first
            selR1 = cell.second
            selC2 = selC1
            selR2 = selR1
            performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            onSelectionChanged?.invoke(true)
            invalidate()
        }

        override fun onScroll(
            e1: MotionEvent?,
            e2: MotionEvent,
            distanceX: Float,
            distanceY: Float,
        ): Boolean {
            if (selecting) {
                val cell = cellAt(e2.x, e2.y) ?: return true
                selC2 = cell.first
                selR2 = cell.second
                invalidate()
                return true
            }
            val mode = forwarding()
            if (mode != AgentTerminalPolicy.Wheel.None) {
                val rowH = drawCellH.coerceAtLeast(1f)
                wheelRemainder += distanceY
                val lines = (wheelRemainder / rowH).toInt()
                if (lines != 0) {
                    val steps = min(abs(lines), 8)
                    // distanceY > 0 = finger up → content toward bottom (same as stock Android terminal view).
                    val towardBottom = lines > 0
                    val applied = if (towardBottom) steps else -steps
                    wheelRemainder -= applied * rowH
                    when (mode) {
                        AgentTerminalPolicy.Wheel.Mouse ->
                            sendWheel(e2.x, e2.y, towardBottom, steps)
                        AgentTerminalPolicy.Wheel.AlternateScroll ->
                            sendAlternateScroll(towardBottom, steps)
                        AgentTerminalPolicy.Wheel.None -> Unit
                    }
                }
                return true
            }
            val rowH = drawCellH.coerceAtLeast(cellHeight.toFloat())
            scrollRemainder += distanceY
            val lines = (scrollRemainder / rowH).toInt()
            if (lines != 0) {
                // Match stock: finger down (distanceY < 0) increases offset into history.
                val applied = if (lines > 0) lines else -abs(lines)
                scrollRemainder -= applied * rowH
                offsetRows = (offsetRows - applied).coerceIn(0, maxOffset())
                invalidate()
            }
            return true
        }
    })

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        setBackgroundColor(canvasColor)
        val fm = textPaint.fontMetrics
        cellWidth = textPaint.measureText("X")
        cellHeight = ceil(fm.descent - fm.ascent + dp(3f).toDouble()).toInt().coerceAtLeast(1)
        baseline = -fm.ascent + dp(1.5f)
        drawCellW = cellWidth
        drawCellH = cellHeight.toFloat()
        drawBaseline = baseline
        topPad = 0
        terminal.onFrame = frameListener
        setOnTouchListener { _, event -> gesture.onTouchEvent(event); true }
    }

    fun bind(terminal: HostTerminal) {
        if (this.terminal !== terminal) {
            this.terminal.onFrame = null
            this.terminal.parkedScroll = offsetRows
            this.terminal = terminal
            offsetRows = terminal.parkedScroll.coerceAtLeast(0)
            clearSelection()
            if (menuActive) {
                menuActive = false
                onMenuChanged?.invoke(false)
            }
        }
        terminal.onFrame = frameListener
        clampOffset()
        invalidate()
    }

    fun selectedText(): String {
        if (!selecting) return ""
        return terminal.selectedText(selC1, selR1, selC2, selR2, offsetRows)
    }

    fun copySelection(clear: Boolean = true): Boolean {
        val text = selectedText()
        if (text.isEmpty()) return false
        val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return false
        clipboard.setPrimaryClip(ClipData.newPlainText("unpeel", text))
        if (clear) clearSelection()
        return true
    }

    fun clearSelection() {
        if (!selecting) return
        selecting = false
        selC1 = -1
        selR1 = -1
        selC2 = -1
        selR2 = -1
        onSelectionChanged?.invoke(false)
        invalidate()
    }

    fun menuKey(name: String): String {
        val application = terminal.applicationCursorKeys
        return when (name) {
            "up" -> if (application) "\u001bOA" else TerminalKeys.UP
            "down" -> if (application) "\u001bOB" else TerminalKeys.DOWN
            "left" -> if (application) "\u001bOD" else TerminalKeys.LEFT
            "right" -> if (application) "\u001bOC" else TerminalKeys.RIGHT
            "enter" -> TerminalKeys.ENTER
            "esc" -> TerminalKeys.ESC
            "tab" -> TerminalKeys.TAB
            else -> ""
        }
    }

    fun dismissKeyboard() {
        val imm = context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        imm.hideSoftInputFromWindow(windowToken, 0)
    }

    fun showKeyboard() {
        requestFocus()
        val imm = context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        imm.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        removeCallbacks(blink)
        post(blink)
    }

    override fun onDetachedFromWindow() {
        removeCallbacks(blink)
        terminal.onFrame = null
        super.onDetachedFromWindow()
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val frame = terminal.frame(offsetRows)
        val cols = frame.cols.coerceAtLeast(1)
        val rows = frame.rows.coerceAtLeast(1)
        if (width <= 0 || height <= 0) return

        val scale = min(width / (cols * cellWidth), height / (rows * cellHeight.toFloat()))
            .coerceAtMost(1.15f)
            .coerceAtLeast(0.55f)
        drawCellW = cellWidth * scale
        drawCellH = cellHeight * scale
        drawOriginX = ((width - cols * drawCellW) / 2f).coerceAtLeast(0f)
        drawOriginY = 0f // top-align like iOS phone terminal
        textPaint.textSize = baseTextSize * scale
        val fm = textPaint.fontMetrics
        drawBaseline = -fm.ascent + (drawCellH - (fm.descent - fm.ascent)) / 2f

        // Clear unused letterbox with canvas color.
        if (drawOriginX > 0.5f || cols * drawCellW < width - 0.5f) {
            bgPaint.color = canvasColor
            canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), bgPaint)
        }

        for (r in 0 until rows) {
            var c = 0
            while (c < cols) {
                val cell = frame.cell(r, c)
                if (cell.ch == '\u0000') {
                    if (selecting && inSelection(c, r)) {
                        val left = drawOriginX + c * drawCellW
                        val top = drawOriginY + r * drawCellH
                        canvas.drawRect(left, top, left + drawCellW, top + drawCellH, selectionPaint)
                    }
                    c++
                    continue
                }
                val span = if (isWideGlyph(cell.ch)) min(2, cols - c) else 1
                drawCell(canvas, r, c, cell, span, selecting && inSelection(c, r))
                c += span
            }
        }

        if (cursorVisible && offsetRows == 0) {
            if (frame.cursorRow in 0 until rows && frame.cursorCol in 0 until cols) {
                val x = drawOriginX + frame.cursorCol * drawCellW
                val y = drawOriginY + frame.cursorRow * drawCellH
                canvas.drawRect(x, y, x + drawCellW, y + drawCellH, cursorPaint)
            }
        }
    }

    private fun drawCell(
        canvas: Canvas,
        row: Int,
        col: Int,
        cell: VtScreen.Cell,
        span: Int,
        selected: Boolean,
    ) {
        val left = drawOriginX + col * drawCellW
        val top = drawOriginY + row * drawCellH
        val right = left + drawCellW * span
        val bottom = top + drawCellH
        bgPaint.color = paintBg(cell.bg)
        canvas.drawRect(left, top, right, bottom, bgPaint)
        if (selected) {
            canvas.drawRect(left, top, right, bottom, selectionPaint)
        }
        if (cell.ch != ' ') {
            textPaint.color = paintFg(cell)
            textPaint.isFakeBoldText = cell.bold && !cell.dim
            textPaint.style = Paint.Style.FILL
            val save = canvas.save()
            canvas.clipRect(left, top, right + 0.5f, bottom)
            drawGlyph(canvas, cell.ch, left, top, span)
            canvas.restoreToCount(save)
        }
    }

    /**
     * Agent TUIs lean on geometric symbols the phone mono font either lacks
     * or hands off to Noto Color Emoji (yellow record / bullet). Match stock
     * Draw a few shapes ourselves, force text presentation, and
     * shrink any leftover glyph that still overflows the cell.
     */
    private fun drawGlyph(canvas: Canvas, ch: Char, left: Float, top: Float, span: Int) {
        when (ch.code) {
            // Claude caret / auto-mode arrows — emoji form is a fat yellow ▶.
            0x23F5, 0x25B6, 0x25B8, 0x25BA -> {
                drawPromptTriangle(canvas, left, top, drawCellW)
                return
            }
            // ⏩ / ⏪ occupy one cell but emoji paints a wide yellow block.
            0x23E9 -> {
                drawPromptTriangle(canvas, left, top, drawCellW * 0.55f)
                drawPromptTriangle(canvas, left + drawCellW * 0.42f, top, drawCellW * 0.55f)
                return
            }
            0x23EA -> {
                drawPromptTriangle(canvas, left, top, drawCellW * 0.55f, pointLeft = true)
                drawPromptTriangle(canvas, left + drawCellW * 0.42f, top, drawCellW * 0.55f, pointLeft = true)
                return
            }
            0x25C0, 0x25C2, 0x25C4 -> {
                drawPromptTriangle(canvas, left, top, drawCellW, pointLeft = true)
                return
            }
            // Record / black circle bullets (⏺ ● ◆ etc.) — avoid emoji rings.
            0x23FA, 0x25CF, 0x2B24, 0x26AB, 0x2B25 -> {
                drawFilledCircle(canvas, left, top, drawCellW * span)
                return
            }
            // Claude status marks (☀ ✻ ※ …) default to color-emoji sunbursts.
            0x2600, 0x2601, 0x203B, 0x2042, 0x2055, 0x2726, 0x2727, 0x2728,
            0x2733, 0x2734, 0x273B, 0x273F, 0x2742, 0x2747, 0x274B, 0x2B50,
            0x2B51, 0x2B52, 0x2B53, 0x2B54 -> {
                drawFilledCircle(canvas, left, top, drawCellW * span)
                return
            }
            0x25CB, 0x25E6, 0x26AA -> {
                drawHollowCircle(canvas, left, top, drawCellW * span)
                return
            }
        }
        // Phone mono fonts either lack Braille or put the ink on the left of
        // the em-square, so adjacent cells look like "wide gap, tight gap".
        // Decode U+28xx ourselves onto a regular 2×4 grid inside the cell.
        if (ch.code in 0x2800..0x28FF) {
            drawBraille(canvas, ch.code, left, top, drawCellW * span)
            return
        }
        val text = textPresentation(ch)
        val budget = drawCellW * span
        val measured = textPaint.measureText(text)
        val nested = canvas.save()
        if (measured > budget + 0.5f && measured > 0.01f) {
            // Stock TerminalRenderer: scale X only so the glyph fits its cells.
            canvas.scale(budget / measured, 1f, left, top)
        }
        canvas.drawText(text, left, top + drawBaseline, textPaint)
        canvas.restoreToCount(nested)
    }

    /** VS15 requests text style for symbols that also have an emoji form. */
    private fun textPresentation(ch: Char): String {
        val cp = ch.code
        if (cp in EMOJI_TEXT_VARIANTS) return ch.toString() + "\uFE0E"
        if (!monoHasGlyph(ch) && cp > 127) return ch.toString() + "\uFE0E"
        return ch.toString()
    }

    private fun monoHasGlyph(ch: Char): Boolean {
        val cached = glyphCache.get(ch.code, 0)
        if (cached != 0) return cached == 1
        val has = textPaint.hasGlyph(ch.toString())
        glyphCache.put(ch.code, if (has) 1 else 2)
        return has
    }

    private fun drawPromptTriangle(
        canvas: Canvas,
        left: Float,
        top: Float,
        cellW: Float,
        pointLeft: Boolean = false,
    ) {
        val insetX = cellW * 0.28f
        val insetY = drawCellH * 0.30f
        shapePath.rewind()
        if (pointLeft) {
            shapePath.moveTo(left + cellW - insetX, top + insetY)
            shapePath.lineTo(left + insetX, top + drawCellH * 0.5f)
            shapePath.lineTo(left + cellW - insetX, top + drawCellH - insetY)
        } else {
            shapePath.moveTo(left + insetX, top + insetY)
            shapePath.lineTo(left + cellW - insetX, top + drawCellH * 0.5f)
            shapePath.lineTo(left + insetX, top + drawCellH - insetY)
        }
        shapePath.close()
        canvas.drawPath(shapePath, textPaint)
    }

    private fun drawFilledCircle(canvas: Canvas, left: Float, top: Float, width: Float) {
        val cx = left + width * 0.5f
        val cy = top + drawCellH * 0.5f
        val radius = min(width, drawCellH) * 0.22f
        canvas.drawCircle(cx, cy, radius, textPaint)
    }

    /**
     * U+2800 Braille Patterns: bit0=dot1 … bit7=dot8. Layout inside one cell:
     * ```
     *  1 4
     *  2 5
     *  3 6
     *  7 8
     * ```
     */
    private fun drawBraille(canvas: Canvas, code: Int, left: Float, top: Float, width: Float) {
        val bits = code - 0x2800
        if (bits == 0) return
        val insetX = width * 0.22f
        val insetY = drawCellH * 0.14f
        val colGap = width - insetX * 2f
        val rowGap = drawCellH - insetY * 2f
        val radius = min(width, drawCellH) * 0.11f
        // Dot number → (col, row), 0-based.
        val dots = intArrayOf(
            /*1*/ 0, 0, /*2*/ 0, 1, /*3*/ 0, 2, /*4*/ 1, 0,
            /*5*/ 1, 1, /*6*/ 1, 2, /*7*/ 0, 3, /*8*/ 1, 3,
        )
        var bit = 0
        while (bit < 8) {
            if ((bits shr bit) and 1 != 0) {
                val col = dots[bit * 2]
                val row = dots[bit * 2 + 1]
                val cx = left + insetX + col * colGap
                val cy = top + insetY + row * (rowGap / 3f)
                canvas.drawCircle(cx, cy, radius, textPaint)
            }
            bit++
        }
    }

    private fun drawHollowCircle(canvas: Canvas, left: Float, top: Float, width: Float) {
        val cx = left + width * 0.5f
        val cy = top + drawCellH * 0.5f
        val radius = min(width, drawCellH) * 0.22f
        val stroke = textPaint.strokeWidth
        val style = textPaint.style
        textPaint.style = Paint.Style.STROKE
        textPaint.strokeWidth = max(1.2f, radius * 0.28f)
        canvas.drawCircle(cx, cy, radius, textPaint)
        textPaint.style = style
        textPaint.strokeWidth = stroke
    }

    private companion object {
        /** Unicode emoji text-variation sequences that agents use as icons. */
        private val EMOJI_TEXT_VARIANTS = intArrayOf(
            0x231A, 0x231B, 0x23E9, 0x23EA, 0x23EB, 0x23EC, 0x23F0, 0x23F3,
            0x23F8, 0x23F9, 0x23FA, 0x25AA, 0x25AB, 0x25B6, 0x25C0, 0x25FB,
            0x25FC, 0x25FD, 0x25FE, 0x2600, 0x2601, 0x260E, 0x2611, 0x2614,
            0x2615, 0x26A0, 0x26A1, 0x26AA, 0x26AB, 0x26BD, 0x26BE, 0x26C4,
            0x26C5, 0x26CE, 0x26D4, 0x26EA, 0x26F2, 0x26F3, 0x26F5, 0x26FA,
            0x26FD, 0x2702, 0x2708, 0x2709, 0x270F, 0x2712, 0x2714, 0x2716,
            0x271D, 0x2721, 0x2728, 0x2733, 0x2734, 0x2744, 0x2747, 0x2753,
            0x2757, 0x2763, 0x2764, 0x27A1, 0x2B05, 0x2B06, 0x2B07, 0x2B1B,
            0x2B1C, 0x2B50, 0x2B55,
        ).toHashSet()

        private fun disableEmojiFallback(paint: Paint) {
            try {
                val method = Paint::class.java.getMethod("setEmojiFallbackEnabled", Boolean::class.javaPrimitiveType)
                method.invoke(paint, false)
            } catch (_: Throwable) {
                // Pre-API 34 / stub without the method — FE0E + shapes cover us.
            }
        }
    }

    private fun paintBg(color: Int): Int {
        val rgb24 = color and 0xFFFFFF
        // Unthemed / default cells follow the session canvas (OpenCode/Grok hex, etc.).
        if (rgb24 == (VtScreen.DEFAULT_BG and 0xFFFFFF) ||
            rgb24 == (terminal.screen.defaultBg and 0xFFFFFF)
        ) {
            return canvasColor
        }
        return rgb(color)
    }

    private fun paintFg(cell: VtScreen.Cell): Int {
        var color = run {
            val rgb24 = cell.fg and 0xFFFFFF
            if (rgb24 == (VtScreen.DEFAULT_FG and 0xFFFFFF) ||
                rgb24 == (terminal.screen.defaultFg and 0xFFFFFF)
            ) {
                rgb(terminal.screen.defaultFg)
            } else {
                rgb(cell.fg)
            }
        }
        if (cell.dim) {
            // SGR 2: blend toward the cell background so muted greys read.
            color = blendRgb(color, paintBg(cell.bg), 0.55f)
        } else if (cell.bold) {
            color = brighten(color, 1.12f)
        }
        return color
    }

    private fun blendRgb(fg: Int, bg: Int, towardBg: Float): Int {
        val t = towardBg.coerceIn(0f, 1f)
        fun ch(v: Int, shift: Int): Int {
            val a = (v shr shift) and 0xff
            val b = (bg shr shift) and 0xff
            return (a + (b - a) * t).toInt().coerceIn(0, 255)
        }
        return 0xFF000000.toInt() or (ch(fg, 16) shl 16) or (ch(fg, 8) shl 8) or ch(fg, 0)
    }

    private fun brighten(color: Int, factor: Float): Int {
        fun ch(shift: Int): Int =
            (((color shr shift) and 0xff) * factor).toInt().coerceIn(0, 255)
        return 0xFF000000.toInt() or (ch(16) shl 16) or (ch(8) shl 8) or ch(0)
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                if (text.isNullOrEmpty()) return true
                var out = text.toString()
                if (controlArmed() && out.length == 1) {
                    val ch = out[0].lowercaseChar()
                    if (ch in 'a'..'z') {
                        out = TerminalKeys.ctrl(ch)
                        onControlUsed()
                    }
                }
                onText?.invoke(out)
                return true
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                if (beforeLength > 0) onText?.invoke(TerminalKeys.BACKSPACE)
                return true
            }

            override fun sendKeyEvent(event: KeyEvent): Boolean {
                if (event.action != KeyEvent.ACTION_DOWN) return true
                return handleKey(event)
            }
        }
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (handleKey(event)) return true
        return super.onKeyDown(keyCode, event)
    }

    private fun handleKey(event: KeyEvent): Boolean {
        val text = when {
            event.isCtrlPressed -> {
                val label = event.displayLabel.lowercaseChar()
                if (label in 'a'..'z') TerminalKeys.ctrl(label) else null
            }
            event.keyCode == KeyEvent.KEYCODE_ENTER -> TerminalKeys.ENTER
            event.keyCode == KeyEvent.KEYCODE_DEL -> TerminalKeys.BACKSPACE
            event.keyCode == KeyEvent.KEYCODE_TAB -> TerminalKeys.TAB
            event.keyCode == KeyEvent.KEYCODE_ESCAPE -> TerminalKeys.ESC
            event.keyCode == KeyEvent.KEYCODE_DPAD_UP -> menuKey("up")
            event.keyCode == KeyEvent.KEYCODE_DPAD_DOWN -> menuKey("down")
            event.keyCode == KeyEvent.KEYCODE_DPAD_LEFT -> menuKey("left")
            event.keyCode == KeyEvent.KEYCODE_DPAD_RIGHT -> menuKey("right")
            else -> {
                val unicode = event.getUnicodeChar(event.metaState)
                if (unicode != 0) unicode.toChar().toString() else null
            }
        } ?: return false
        onText?.invoke(text)
        return true
    }

    private fun publishMenu() {
        val active = AgentTerminalPolicy.viewportHasMenuPrompt(terminal.visibleText())
        if (active == menuActive) return
        menuActive = active
        onMenuChanged?.invoke(active)
    }

    private fun forwarding(): AgentTerminalPolicy.Wheel =
        AgentTerminalPolicy.wheelForwarding(
            modesKnown = terminal.hostModeSnapshot,
            mouseTracking = terminal.mouseTrackingActive,
            alternateScreen = terminal.alternateBufferActive,
            sawMouseOrAlternateDisable = terminal.sawMouseOrAlternateDisable,
            prefersRemoteMouseWheel = prefersRemoteWheel,
            echoesMouseWheel = echoesMouseWheel,
        )

    private fun sendAlternateScroll(towardBottom: Boolean, steps: Int) {
        val key = menuKey(if (towardBottom) "down" else "up")
        if (key.isNotEmpty()) onText?.invoke(key.repeat(steps.coerceAtLeast(1)))
    }

    /**
     * xterm SGR wheel. Button 64 = up, 65 = down. Used for Claude/Grok/
     * OpenCode (and any program with DECSET mouse on). cursor-agent uses
     * [sendAlternateScroll] instead — SGR lands in its composer.
     */
    private fun sendWheel(x: Float, y: Float, towardBottom: Boolean, steps: Int) {
        if (steps <= 0) return
        val cell = cellAt(x, y)
        val column = ((cell?.first ?: 0) + 1).coerceIn(1, terminal.columns.coerceAtLeast(1))
        val row = ((cell?.second ?: 0) + 1).coerceIn(1, terminal.rows.coerceAtLeast(1))
        val button = if (towardBottom) 65 else 64
        onText?.invoke("\u001b[<$button;$column;${row}M".repeat(steps))
    }

    /** Left press then release at the tapped cell. Column/row are 1-based. */
    private fun sendClick(event: MotionEvent) {
        val cell = cellAt(event.x, event.y) ?: return
        val column = (cell.first + 1).coerceIn(1, terminal.columns.coerceAtLeast(1))
        val row = (cell.second + 1).coerceIn(1, terminal.rows.coerceAtLeast(1))
        onText?.invoke("\u001b[<0;$column;${row}M\u001b[<0;$column;${row}m")
    }

    /**
     * Claude pins a jump chip just above the composer when its own transcript
     * is scrolled up. Match iOS `viewportHasTuiJumpHint`, plus the phone
     * "(click)" wording Claude uses when mouse is available.
     */
    private fun handleTuiJumpTap(event: MotionEvent): Boolean {
        if (event.y < height * 0.45f) return false
        val text = terminal.visibleText()
        val tail = text.lineSequence().toList().takeLast(15).joinToString("\n")
        val clickJump = tail.contains("Jump to bottom (click)") ||
            Regex("""\d+ new messages? \(click\)""").containsMatchIn(tail)
        val keyJump = tail.contains("Jump to bottom (ctrl+End)") ||
            Regex("""\d+ new messages? \(ctrl\+End\)""").containsMatchIn(tail)
        if (!clickJump && !keyJump) return false
        offsetRows = 0
        scrollRemainder = 0f
        invalidate()
        dismissKeyboard()
        if (clickJump) {
            // Chip sits in the lower grid; a miss on letterbox still needs a
            // report — aim at the last few rows where Claude pins it.
            if (cellAt(event.x, event.y) != null) {
                sendClick(event)
            } else {
                val column = (terminal.columns / 2).coerceAtLeast(1)
                val row = (terminal.rows - 2).coerceAtLeast(1)
                onText?.invoke("\u001b[<0;$column;${row}M\u001b[<0;$column;${row}m")
            }
        }
        // Same key iOS synthesizes for the ctrl+End chip. Claude accepts it
        // for virtual-scroll jump even when the hint says "(click)".
        onText?.invoke("\u001b[1;5F")
        return true
    }

    private fun tapTargetsCursor(event: MotionEvent): Boolean {
        val cell = cellAt(event.x, event.y) ?: return false
        val (cr, _) = terminal.cursor()
        return abs(cell.second - cr) <= 1
    }

    private fun cellAt(x: Float, y: Float): Pair<Int, Int>? {
        val w = drawCellW.coerceAtLeast(1f)
        val h = drawCellH.coerceAtLeast(1f)
        val c = ((x - drawOriginX) / w).toInt()
        val r = ((y - drawOriginY) / h).toInt()
        if (c !in 0 until terminal.columns || r !in 0 until terminal.rows) return null
        return c to r
    }

    private fun inSelection(c: Int, r: Int): Boolean {
        val top = min(selR1, selR2)
        val bottom = max(selR1, selR2)
        if (r !in top..bottom) return false
        val left = if (r == top) min(selC1, selC2) else 0
        val right = if (r == bottom) max(selC1, selC2) else terminal.columns - 1
        return c in left..right
    }

    private fun maxOffset(): Int = terminal.scrollbackLines()

    private fun clampOffset() {
        offsetRows = offsetRows.coerceIn(0, maxOffset())
    }

    private fun rgb(color: Int): Int =
        if (color ushr 24 == 0) 0xFF000000.toInt() or (color and 0xFFFFFF) else color

    private fun isWideGlyph(ch: Char): Boolean {
        val code = ch.code
        return code in 0x1100..0x115F ||
            code in 0x2E80..0xA4CF ||
            code in 0xAC00..0xD7A3 ||
            code in 0xF900..0xFAFF ||
            code in 0xFE10..0xFE19 ||
            code in 0xFE30..0xFE6F ||
            code in 0xFF00..0xFF60 ||
            code in 0xFFE0..0xFFE6
    }

    private fun dp(v: Float): Float = v * resources.displayMetrics.density
}
