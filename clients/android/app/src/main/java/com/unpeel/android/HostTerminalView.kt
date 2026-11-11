package com.unpeel.android

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Canvas
import android.text.InputType
import android.util.TypedValue
import android.view.KeyEvent
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import com.termux.terminal.TerminalEmulator
import com.termux.view.TerminalMetrics
import com.termux.view.TerminalRenderer
import com.unpeel.controller.TerminalInput
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * Draws a Host terminal and takes the keyboard. Committed characters go
 * straight to the session. This view never changes the PTY size.
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
    /** Raw terminal bytes. Enter is already `\r`. A multi-line paste is bracketed and not submitted. */
    var onText: ((String) -> Unit)? = null
    /** Sticky control from the key row. Read when a letter is committed. */
    var controlArmed: () -> Boolean = { false }
    var onControlUsed: () -> Unit = {}

    private var drawn: TerminalEmulator? = null
    private var topRow = 0
    /** Rows above the bottom of the view. 0 keeps the cursor row on screen. */
    private var offsetRows = 0
    private var panX = 0
    /** Finger travel not yet worth a whole row, in GestureDetector distanceY sign. */
    private var scrollRemainder = 0f
    /** Rows already sent to the host that its next frame has not drawn yet. */
    private var pendingRemoteRows = 0
    private var flingVelocity = 0f
    private var selecting = false
    private var selX1 = -1
    private var selY1 = -1
    private var selX2 = -1
    private var selY2 = -1
    private var blinkOn = true

    private val frameListener: () -> Unit = {
        // The host picture already includes the rows we asked for.
        pendingRemoteRows = 0
        if (terminal.emulator.isMouseTrackingActive() || terminal.emulator.isAlternateBufferActive()) {
            scrollRemainder = 0f
        }
        noteEmulator(terminal.emulator)
        clampOffset(terminal.emulator)
        postInvalidateOnAnimation()
    }

    private val flingTick = object : Runnable {
        override fun run() {
            val emulator = terminal.emulator
            if (flingVelocity == 0f || emulator.isMouseTrackingActive() || emulator.isAlternateBufferActive()) {
                flingVelocity = 0f
                return
            }
            scrollRemainder += flingVelocity * 0.016f
            applyScrollLines(emulator, null)
            flingVelocity *= 0.90f
            if (kotlin.math.abs(flingVelocity) < 50f) flingVelocity = 0f
            invalidate()
            if (flingVelocity != 0f) postOnAnimation(this)
        }
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
        override fun onDown(e: MotionEvent): Boolean {
            flingVelocity = 0f
            removeCallbacks(flingTick)
            return true
        }

        override fun onSingleTapUp(e: MotionEvent): Boolean {
            if (selecting) {
                clearSelection()
                return true
            }
            val emulator = terminal.emulator
            if (!emulator.isMouseTrackingActive()) {
                showKeyboard()
                return true
            }
            // The program owns the click (Stop, menus, links). The keyboard
            // comes up only when the tap lands on the cursor row.
            sendClick(emulator, e)
            if (tapTargetsCursor(emulator, e)) showKeyboard() else dismissKeyboard()
            return true
        }

        override fun onLongPress(e: MotionEvent) {
            val emulator = terminal.emulator
            if (tapTargetsCursor(emulator, e) && pasteClipboard()) {
                clearSelection()
                return
            }
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
            val emulator = terminal.emulator
            // Finger down (negative distance) moves toward older rows.
            // The picture shifts with the finger immediately; a whole row
            // is committed only after the finger has travelled one cell.
            if (distanceY != 0f && cellHeight > 0) {
                scrollRemainder += distanceY
                applyScrollLines(emulator, e2)
            }
            panX -= distanceX.toInt()
            horizontalShift(emulator)
            invalidate()
            return true
        }

        override fun onFling(e1: MotionEvent?, e2: MotionEvent, velocityX: Float, velocityY: Float): Boolean {
            if (selecting || cellHeight <= 0) return false
            val emulator = terminal.emulator
            // velocityY is downward. distanceY (and scrollRemainder) grow when the finger moves up.
            if (emulator.isMouseTrackingActive() || emulator.isAlternateBufferActive()) {
                val lines = (-velocityY / cellHeight / 6f).toInt()
                if (lines == 0) return false
                val steps = min(kotlin.math.abs(lines), 8)
                val applied = if (lines > 0) steps else -steps
                pendingRemoteRows += applied
                if (emulator.isMouseTrackingActive()) sendWheel(emulator, e2, applied > 0, steps)
                else sendAlternateScroll(emulator, applied > 0, steps)
                invalidate()
                return true
            }
            flingVelocity = -velocityY
            postOnAnimation(flingTick)
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
        isFocusable = true
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

    /** Copies the selection onto the system clipboard and clears it. */
    fun copySelection(): Boolean {
        val text = selectedText()
        if (text.isEmpty()) return false
        val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return false
        clipboard.setPrimaryClip(ClipData.newPlainText("unpeel", text))
        clearSelection()
        return true
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

    fun showKeyboard() {
        isFocusableInTouchMode = true
        requestFocus()
        post {
            context.getSystemService(InputMethodManager::class.java)
                ?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        }
    }

    fun dismissKeyboard() {
        context.getSystemService(InputMethodManager::class.java)
            ?.hideSoftInputFromWindow(windowToken, 0)
        clearFocus()
        isFocusableInTouchMode = false
    }

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // No password variation: that flag is what makes Android open the
        // secure keyboard and hide dictation, emoji, and a normal IME.
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE or
            EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_FLAG_NO_FULLSCREEN
        outAttrs.initialSelStart = 0
        outAttrs.initialSelEnd = 0
        return TerminalInputConnection(this)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (!isFocused) return super.onKeyDown(keyCode, event)
        return handleTerminalKey(keyCode, event) || super.onKeyDown(keyCode, event)
    }

    internal fun handleTerminalKey(keyCode: Int, event: KeyEvent): Boolean {
        val mapped = when (keyCode) {
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> "\r"
            KeyEvent.KEYCODE_DEL -> "\u007f"
            KeyEvent.KEYCODE_FORWARD_DEL -> "\u001b[3~"
            KeyEvent.KEYCODE_TAB -> if (event.isShiftPressed) "\u001b[Z" else "\t"
            KeyEvent.KEYCODE_ESCAPE -> "\u001b"
            KeyEvent.KEYCODE_DPAD_UP -> "\u001b[A"
            KeyEvent.KEYCODE_DPAD_DOWN -> "\u001b[B"
            KeyEvent.KEYCODE_DPAD_RIGHT -> "\u001b[C"
            KeyEvent.KEYCODE_DPAD_LEFT -> "\u001b[D"
            else -> null
        }
        if (mapped != null) {
            deliver(mapped)
            return true
        }
        val ch = event.unicodeChar
        if (ch > 0) {
            emitCommitted(ch.toChar().toString())
            return true
        }
        return false
    }

    /** A finished IME commit. A lone newline is Enter. A block with newlines is a paste, not a submit. */
    internal fun emitCommitted(text: String) {
        val normalized = text.replace("\r\n", "\n").replace('\r', '\n')
        if (normalized.isEmpty()) return
        if (normalized.all { it == '\n' }) {
            deliver("\r".repeat(normalized.length))
            return
        }
        if ('\n' in normalized) {
            deliver(TerminalInput.paste(TerminalInput.sanitize(normalized)))
            return
        }
        if (controlArmed() && normalized.length == 1 && normalized[0].isLetter()) {
            val code = normalized[0].lowercaseChar().code - 'a'.code + 1
            if (code in 1..26) {
                deliver(code.toChar().toString())
                onControlUsed()
                return
            }
        }
        deliver(normalized)
    }

    private fun deliver(text: String) {
        if (text.isNotEmpty()) onText?.invoke(text)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        bind(terminal)
        removeCallbacks(blink)
        postDelayed(blink, 500)
    }

    override fun onDetachedFromWindow() {
        removeCallbacks(blink)
        removeCallbacks(flingTick)
        if (terminal.onFrame === frameListener) terminal.onFrame = null
        super.onDetachedFromWindow()
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
            parent?.requestDisallowInterceptTouchEvent(true)
        }
        gesture.onTouchEvent(event)
        return true
    }

    override fun onDraw(canvas: Canvas) {
        val emulator = terminal.emulator
        noteEmulator(emulator)
        canvas.drawColor(CANVAS)
        canvas.save()
        val placed = place(emulator)
        topRow = placed.first
        canvas.translate(horizontalShift(emulator).toFloat(), placed.second.toFloat())
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
        offsetRows = 0
        panX = 0
        scrollRemainder = 0f
        pendingRemoteRows = 0
        flingVelocity = 0f
        if (selecting) clearSelection()
    }

    /**
     * Turns accumulated finger travel into whole rows. Remote rows are sent
     * to the host; the local buffer moves `offsetRows`. Pixels short of a
     * row stay in `scrollRemainder` and `place` draws them.
     */
    private fun applyScrollLines(emulator: TerminalEmulator, event: MotionEvent?) {
        if (cellHeight <= 0) return
        val lines = (scrollRemainder / cellHeight).toInt()
        if (lines == 0) return
        val remote = emulator.isMouseTrackingActive() || emulator.isAlternateBufferActive()
        val steps = if (remote) min(kotlin.math.abs(lines), 24) else kotlin.math.abs(lines)
        val applied = if (lines > 0) steps else -steps
        scrollRemainder -= applied * cellHeight
        when {
            emulator.isMouseTrackingActive() && event != null -> {
                pendingRemoteRows += applied
                sendWheel(emulator, event, applied > 0, steps)
            }
            emulator.isAlternateBufferActive() -> {
                pendingRemoteRows += applied
                sendAlternateScroll(emulator, applied > 0, steps)
            }
            else -> {
                offsetRows = (offsetRows - applied).coerceIn(0, maxOffset(emulator))
                clampOffset(emulator)
            }
        }
    }

    /** Rows of the current screen that sit above the view, plus saved scrollback. */
    private fun maxOffset(emulator: TerminalEmulator): Int {
        val history = if (emulator.isAlternateBufferActive) 0 else emulator.screen.activeTranscriptRows
        val hidden = (emulator.mRows - visibleRows()).coerceAtLeast(0)
        return hidden + history
    }

    private fun visibleRows(): Int {
        if (cellHeight <= 0) return 1
        return ((height - topPad) / cellHeight).coerceAtLeast(1)
    }

    private fun clampOffset(emulator: TerminalEmulator) {
        offsetRows = offsetRows.coerceIn(0, maxOffset(emulator))
    }

    /** topRow for the renderer, and the Y translation that puts that row in view. */
    private fun place(emulator: TerminalEmulator): Pair<Int, Int> {
        clampOffset(emulator)
        val remote = emulator.isMouseTrackingActive() || emulator.isAlternateBufferActive()
        if (!remote) {
            if (offsetRows == 0 && scrollRemainder > 0f) scrollRemainder = 0f
            if (offsetRows == maxOffset(emulator) && scrollRemainder < 0f) scrollRemainder = 0f
        }
        val hidden = (emulator.mRows - visibleRows()).coerceAtLeast(0)
        val intoHistory = (offsetRows - hidden).coerceAtLeast(0)
        val row = -intoHistory
        val base = if (intoHistory > 0) 0 else minPanY(emulator) + offsetRows * cellHeight
        // Negative remainder is a finger moving down, so the picture moves down with it.
        val slack = Math.round(-scrollRemainder) - pendingRemoteRows * cellHeight
        return row to (base + slack)
    }

    /** Left press then release at the tapped cell. Column and row are 1-based. */
    private fun sendClick(emulator: TerminalEmulator, event: MotionEvent) {
        val cell = cellAt(event.x, event.y) ?: return
        val column = (cell.first + 1).coerceIn(1, emulator.mColumns)
        val row = (cell.second + 1).coerceIn(1, emulator.mRows)
        val payload = if (emulator.isMouseProtocolSgr()) {
            "\u001b[<0;$column;${row}M\u001b[<0;$column;${row}m"
        } else {
            val bytes = byteArrayOf(
                0x1b, '['.code.toByte(), 'M'.code.toByte(),
                32, (32 + column).toByte(), (32 + row).toByte(),
                0x1b, '['.code.toByte(), 'M'.code.toByte(),
                (32 + 3).toByte(), (32 + column).toByte(), (32 + row).toByte(),
            )
            bytes.toString(Charsets.ISO_8859_1)
        }
        onText?.invoke(payload)
    }

    /** Bracketed paste of the system clipboard. False when there is nothing to paste. */
    private fun pasteClipboard(): Boolean {
        val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return false
        val text = try {
            clipboard.primaryClip?.takeIf { it.itemCount > 0 }
                ?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty()
        } catch (_: Exception) {
            return false
        }
        val sanitized = TerminalInput.sanitize(text)
        if (sanitized.isEmpty()) return false
        onText?.invoke(TerminalInput.paste(sanitized))
        return true
    }

    private fun tapTargetsCursor(emulator: TerminalEmulator, event: MotionEvent): Boolean {
        val cell = cellAt(event.x, event.y) ?: return false
        return kotlin.math.abs(cell.second - emulator.cursorRow) <= 1
    }

    /** Finger moving up (`towardBottom`) reveals newer rows. */
    private fun sendAlternateScroll(emulator: TerminalEmulator, towardBottom: Boolean, steps: Int) {
        val key = if (emulator.isCursorKeysApplicationMode()) {
            if (towardBottom) "\u001bOB" else "\u001bOA"
        } else {
            if (towardBottom) "\u001b[B" else "\u001b[A"
        }
        onText?.invoke(key.repeat(steps))
    }

    private fun sendWheel(emulator: TerminalEmulator, event: MotionEvent, towardBottom: Boolean, steps: Int) {
        val cell = cellAt(event.x, event.y)
        val column = ((cell?.first ?: 0) + 1).coerceIn(1, emulator.mColumns)
        val row = ((cell?.second ?: 0) + 1).coerceIn(1, emulator.mRows)
        val button = if (towardBottom) TerminalEmulator.MOUSE_WHEELDOWN_BUTTON else TerminalEmulator.MOUSE_WHEELUP_BUTTON
        val payload = if (emulator.isMouseProtocolSgr()) {
            "\u001b[<$button;$column;${row}M".repeat(steps)
        } else {
            val code = ByteArray(6 * steps)
            var index = 0
            repeat(steps) {
                code[index++] = 0x1b
                code[index++] = '['.code.toByte()
                code[index++] = 'M'.code.toByte()
                code[index++] = (32 + button).toByte()
                code[index++] = (32 + column).toByte()
                code[index++] = (32 + row).toByte()
            }
            code.toString(Charsets.ISO_8859_1)
        }
        onText?.invoke(payload)
    }

    /** Pins the cursor row to the bottom of the view. The finger never moves this. */
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
        val placed = place(emulator)
        val contentX = x - horizontalShift(emulator)
        val contentY = y - placed.second
        val localRow = ((contentY - topPad) / cellHeight).toInt()
        if (localRow < 0 || localRow >= emulator.mRows) return null
        val col = (contentX / cellWidth).toInt()
        if (col < 0 || col >= emulator.mColumns) return null
        return col to (placed.first + localRow)
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

/** Forwards a finished commit. Composing text stays here until `commitText`. */
private class TerminalInputConnection(private val terminal: HostTerminalView) : BaseInputConnection(terminal, false) {
    private var composingLength = 0

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        composingLength = 0
        val raw = text?.toString().orEmpty()
        if (raw.isNotEmpty()) terminal.emitCommitted(raw)
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        composingLength = text?.length ?: 0
        return true
    }

    override fun finishComposingText(): Boolean {
        composingLength = 0
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        if (composingLength > 0) {
            composingLength = (composingLength - beforeLength).coerceAtLeast(0)
            return true
        }
        val event = KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL)
        repeat(beforeLength.coerceAtLeast(0)) { terminal.handleTerminalKey(KeyEvent.KEYCODE_DEL, event) }
        return true
    }

    override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int): Boolean =
        deleteSurroundingText(beforeLength, afterLength)

    override fun sendKeyEvent(event: KeyEvent): Boolean {
        if (event.action == KeyEvent.ACTION_DOWN) terminal.handleTerminalKey(event.keyCode, event)
        return true
    }

    override fun getTextBeforeCursor(length: Int, flags: Int): CharSequence = ""

    override fun getTextAfterCursor(length: Int, flags: Int): CharSequence = ""
}
