package com.termit.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.text.InputType
import android.util.AttributeSet
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import com.termit.TerminalSession
import com.termit.vt.Cell
import kotlin.math.max
import kotlin.math.min

/**
 * Renders [TerminalSession] screen buffer and forwards keyboard input to the transport.
 */
class TerminalView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0,
) : View(context, attrs, defStyleAttr) {
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Typeface.MONOSPACE
        textSize = sp(14f)
    }
    private val bgPaint = Paint()
    private val cursorPaint = Paint().apply {
        color = 0x99FFFFFF.toInt()
        style = Paint.Style.FILL
    }

    private var session: TerminalSession? = null
    private var cellWidth = 0f
    private var cellHeight = 0f
    private var cellBaseline = 0f
    private var cols = 80
    private var rows = 24
    private var scrollOffset = 0
    private var cursorVisible = true

    private val sessionListener = object : TerminalSession.Listener {
        override fun onScreenUpdate() {
            scrollOffset = 0
            postInvalidateOnAnimation()
        }
    }

    private val blinkRunnable = object : Runnable {
        override fun run() {
            cursorVisible = !cursorVisible
            invalidate()
            postDelayed(this, 530L)
        }
    }

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        setBackgroundColor(AnsiColors.resolve(AnsiColors.INDEX_BG, AnsiColors.buildDefaultPalette()))
        measureCell()
    }

    fun attach(session: TerminalSession) {
        this.session?.removeListener(sessionListener)
        this.session = session
        session.addListener(sessionListener)
        requestLayout()
        invalidate()
    }

    fun detach() {
        session?.removeListener(sessionListener)
        session = null
    }

    fun showKeyboard() {
        requestFocus()
        val imm = context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        imm.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        removeCallbacks(blinkRunnable)
        post(blinkRunnable)
    }

    override fun onDetachedFromWindow() {
        removeCallbacks(blinkRunnable)
        super.onDetachedFromWindow()
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        measureCell()
        val w = MeasureSpec.getSize(widthMeasureSpec)
        val h = MeasureSpec.getSize(heightMeasureSpec)
        cols = max(1, (w / cellWidth).toInt())
        rows = max(1, (h / cellHeight).toInt())
        session?.resize(cols, rows)
        setMeasuredDimension(w, h)
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val emu = session?.emulator ?: return
        val screen = emu.screen
        val maxScroll = screen.scrollbackLines
        scrollOffset = scrollOffset.coerceIn(0, maxScroll)

        for (r in 0 until min(rows, screen.rows)) {
            for (c in 0 until min(cols, screen.cols)) {
                val cell = screen.visibleCell(r, c, scrollOffset)
                drawCell(canvas, r, c, cell)
            }
        }

        if (cursorVisible && scrollOffset == 0) {
            val x = emu.cursorCol * cellWidth
            val y = emu.cursorRow * cellHeight
            canvas.drawRect(x, y, x + cellWidth, y + cellHeight, cursorPaint)
            if (emu.cursorCol < screen.cols && emu.cursorRow < screen.rows) {
                drawCell(canvas, emu.cursorRow, emu.cursorCol, screen.cell(emu.cursorRow, emu.cursorCol), forceFg = true)
            }
        }
    }

    private fun drawCell(canvas: Canvas, row: Int, col: Int, cell: Cell, forceFg: Boolean = false) {
        val palette = session?.emulator?.colors ?: AnsiColors.buildDefaultPalette()
        var fg = AnsiColors.resolve(cell.fg, palette, bold = cell.bold, dim = cell.dim)
        var bg = AnsiColors.resolve(cell.bg, palette)
        if (cell.inverse) {
            val tmp = fg
            fg = bg
            bg = tmp
        }
        val left = col * cellWidth
        val top = row * cellHeight
        bgPaint.color = bg
        canvas.drawRect(left, top, left + cellWidth, top + cellHeight, bgPaint)
        if (cell.char != ' ' || forceFg) {
            textPaint.color = fg
            textPaint.isFakeBoldText = cell.bold
            textPaint.isUnderlineText = cell.underline
            canvas.drawText(cell.char.toString(), left, top + cellBaseline, textPaint)
        }
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.action == MotionEvent.ACTION_UP) {
            showKeyboard()
            performClick()
        }
        return true
    }

    override fun performClick(): Boolean {
        super.performClick()
        return true
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, true) {
            override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                if (!text.isNullOrEmpty()) session?.write(text.toString())
                return true
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                if (beforeLength > 0) session?.write(TerminalKeys.BACKSPACE)
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
        val s = session ?: return false
        if (event.isCtrlPressed) {
            val label = event.displayLabel.lowercaseChar()
            if (label in 'a'..'z') {
                s.write(TerminalKeys.ctrl(label))
                return true
            }
        }
        when (event.keyCode) {
            KeyEvent.KEYCODE_ENTER -> s.write(TerminalKeys.ENTER)
            KeyEvent.KEYCODE_DEL -> s.write(TerminalKeys.BACKSPACE)
            KeyEvent.KEYCODE_TAB -> s.write(TerminalKeys.TAB)
            KeyEvent.KEYCODE_ESCAPE -> s.write(TerminalKeys.ESC)
            KeyEvent.KEYCODE_DPAD_UP -> s.write(TerminalKeys.UP)
            KeyEvent.KEYCODE_DPAD_DOWN -> s.write(TerminalKeys.DOWN)
            KeyEvent.KEYCODE_DPAD_LEFT -> s.write(TerminalKeys.LEFT)
            KeyEvent.KEYCODE_DPAD_RIGHT -> s.write(TerminalKeys.RIGHT)
            KeyEvent.KEYCODE_MOVE_HOME -> s.write(TerminalKeys.HOME)
            KeyEvent.KEYCODE_MOVE_END -> s.write(TerminalKeys.END)
            KeyEvent.KEYCODE_PAGE_UP -> s.write(TerminalKeys.PAGE_UP)
            KeyEvent.KEYCODE_PAGE_DOWN -> s.write(TerminalKeys.PAGE_DOWN)
            KeyEvent.KEYCODE_FORWARD_DEL -> s.write(TerminalKeys.DELETE)
            else -> {
                val unicode = event.getUnicodeChar(event.metaState)
                if (unicode != 0) {
                    s.write(unicode.toChar().toString())
                } else return false
            }
        }
        return true
    }

    private fun measureCell() {
        cellWidth = textPaint.measureText("M")
        val fm = textPaint.fontMetrics
        cellHeight = fm.descent - fm.ascent + dp(2f)
        cellBaseline = -fm.ascent + dp(1f)
    }

    private fun sp(v: Float): Float = v * resources.displayMetrics.scaledDensity
    private fun dp(v: Float): Float = v * resources.displayMetrics.density
}
