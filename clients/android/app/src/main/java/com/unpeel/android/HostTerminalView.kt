package com.unpeel.android

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.util.TypedValue
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.View
import com.termux.terminal.TerminalEmulator
import com.termux.view.TerminalMetrics
import com.termux.view.TerminalRenderer
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * Draws a Host terminal. Not a text editor: the composer owns the keyboard,
 * and this view never changes the PTY size.
 */
@SuppressLint("ViewConstructor", "ClickableViewAccessibility")
class HostTerminalView(
    context: Context,
    private var terminal: HostTerminal,
) : View(context) {
    private val renderer: TerminalRenderer
    val cellWidth: Float
    val cellHeight: Int
    val topPad: Int

    var onSelectionChanged: ((Boolean) -> Unit)? = null

    private var drawn: TerminalEmulator? = null
    private var topRow = 0
    private var panX = 0
    private var panY = 0
    private var followBottom = true
    private var scrollRemainder = 0f
    private var selecting = false
    private var selX1 = -1
    private var selY1 = -1
    private var selX2 = -1
    private var selY2 = -1
    private var blinkOn = true

    private val frameListener: () -> Unit = {
        noteEmulator(terminal.emulator)
        if (topRow != 0) clampTop()
        postInvalidateOnAnimation()
    }

    private val blink = object : Runnable {
        override fun run() {
            blinkOn = !blinkOn
            terminal.emulator.setCursorBlinkState(blinkOn)
            invalidate()
            postDelayed(this, 500)
        }
    }

    private val gesture = GestureDetector(context, object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent): Boolean = true

        override fun onLongPress(e: MotionEvent) {
            val cell = cellAt(e.x, e.y) ?: return
            selX1 = cell.first
            selY1 = cell.second
            selX2 = cell.first
            selY2 = cell.second
            if (!selecting) {
                selecting = true
                onSelectionChanged?.invoke(true)
            }
            invalidate()
        }

        override fun onScroll(e1: MotionEvent?, e2: MotionEvent, distanceX: Float, distanceY: Float): Boolean {
            if (selecting) {
                val cell = cellAt(e2.x, e2.y) ?: return true
                selX2 = cell.first
                selY2 = cell.second
                invalidate()
                return true
            }
            // Finger down (negative distance) reveals rows above. Spend that
            // inside the current screen before walking into scrollback.
            val emulator = terminal.emulator
            val minY = minPanY(emulator)
            if (distanceY < 0 && panY < 0) {
                panY = (panY - distanceY.toInt()).coerceAtMost(0)
            } else if (distanceY > 0 && topRow < 0) {
                scrollRemainder += distanceY
                val lines = (scrollRemainder / cellHeight).toInt()
                if (lines != 0) {
                    topRow += lines
                    scrollRemainder -= lines * cellHeight
                    clampTop()
                }
            } else if (distanceY < 0 && panY >= 0) {
                scrollRemainder += distanceY
                val lines = (scrollRemainder / cellHeight).toInt()
                if (lines != 0) {
                    topRow += lines
                    scrollRemainder -= lines * cellHeight
                    clampTop()
                }
            } else {
                panY = (panY - distanceY.toInt()).coerceIn(minY, 0)
            }
            followBottom = topRow == 0 && panY <= minY
            panX -= distanceX.toInt()
            horizontalShift(emulator)
            invalidate()
            return true
        }
    })

    init {
        val textPx = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 13f, resources.displayMetrics)
            .roundToInt()
            .coerceAtLeast(8)
        val metrics = TerminalMetrics(textPx)
        renderer = metrics.renderer
        cellWidth = metrics.fontWidth.coerceAtLeast(1f)
        cellHeight = metrics.fontLineSpacing.coerceAtLeast(1)
        topPad = metrics.fontLineSpacingAndAscent
        setBackgroundColor(CANVAS)
        importantForAutofill = IMPORTANT_FOR_AUTOFILL_NO
        isFocusable = false
        isFocusableInTouchMode = false
        isClickable = true
        bind(terminal)
    }

    fun bind(terminal: HostTerminal) {
        this.terminal = terminal
        terminal.onFrame = frameListener
        terminal.updateCellSize(cellWidth.roundToInt(), cellHeight)
        noteEmulator(terminal.emulator)
    }

    fun selectedText(): String {
        val args = selectionArgs() ?: return ""
        return runCatching {
            terminal.emulator.getSelectedText(args[2], args[0], args[3], args[1])
        }.getOrDefault("")
    }

    fun clearSelection() {
        if (!selecting) return
        selecting = false
        selX1 = -1
        selY1 = -1
        selX2 = -1
        selY2 = -1
        onSelectionChanged?.invoke(false)
        invalidate()
    }

    override fun onCheckIsTextEditor(): Boolean = false

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        bind(terminal)
        removeCallbacks(blink)
        postDelayed(blink, 500)
    }

    override fun onDetachedFromWindow() {
        removeCallbacks(blink)
        if (terminal.onFrame === frameListener) terminal.onFrame = null
        super.onDetachedFromWindow()
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        gesture.onTouchEvent(event)
        if (event.actionMasked == MotionEvent.ACTION_UP || event.actionMasked == MotionEvent.ACTION_CANCEL) {
            scrollRemainder = 0f
        }
        return true
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        if (followBottom) panY = minPanY(terminal.emulator)
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val emulator = terminal.emulator
        noteEmulator(emulator)
        if (followBottom) panY = minPanY(emulator)
        canvas.drawColor(CANVAS)
        canvas.save()
        canvas.translate(horizontalShift(emulator).toFloat(), panY.toFloat())
        val sel = selectionArgs()
        if (sel == null) {
            renderer.render(emulator, canvas, topRow, -1, -1, -1, -1)
        } else {
            renderer.render(emulator, canvas, topRow, sel[0], sel[1], sel[2], sel[3])
        }
        canvas.restore()
    }

    private fun noteEmulator(emulator: TerminalEmulator) {
        if (drawn === emulator) return
        drawn = emulator
        topRow = 0
        panX = 0
        panY = 0
        followBottom = true
        scrollRemainder = 0f
        if (selecting) clearSelection()
    }

    private fun clampTop() {
        val emulator = terminal.emulator
        val history = if (emulator.isAlternateBufferActive) 0 else emulator.screen.activeTranscriptRows
        if (topRow < -history) topRow = -history
        if (topRow > 0) topRow = 0
    }

    /** 0 shows the top row. Negative values keep the cursor on screen. */
    private fun minPanY(emulator: TerminalEmulator): Int {
        val contentHeight = topPad + emulator.mRows * cellHeight
        return min(0, height - contentHeight)
    }

    private fun horizontalShift(emulator: TerminalEmulator): Int {
        val contentWidth = (emulator.mColumns * cellWidth).roundToInt()
        val minPan = min(0, width - contentWidth)
        if (panX < minPan) panX = minPan
        if (panX > 0) panX = 0
        return panX
    }

    private fun cellAt(x: Float, y: Float): Pair<Int, Int>? {
        val emulator = terminal.emulator
        val contentX = x - horizontalShift(emulator)
        val contentY = y - panY
        val localRow = ((contentY - topPad) / cellHeight).toInt()
        if (localRow < 0 || localRow >= emulator.mRows) return null
        val col = (contentX / cellWidth).toInt()
        if (col < 0 || col >= emulator.mColumns) return null
        return col to (topRow + localRow)
    }

    /** y1, y2, x1, x2 in emulator coordinates, ordered. */
    private fun selectionArgs(): IntArray? {
        if (!selecting) return null
        val swap = selY1 > selY2 || (selY1 == selY2 && selX1 > selX2)
        return if (swap) {
            intArrayOf(selY2, selY1, selX2, selX1)
        } else {
            intArrayOf(selY1, selY2, selX1, selX2)
        }
    }

    private companion object {
        const val CANVAS = 0xFF1A1B1D.toInt()
    }
}
