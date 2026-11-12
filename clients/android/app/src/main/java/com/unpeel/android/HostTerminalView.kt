package com.unpeel.android

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.text.InputType
import android.util.TypedValue
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import android.view.ViewConfiguration
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import com.termit.ui.TerminalMetrics
import com.termit.vt.VtEmulator
import com.unpeel.controller.AgentTerminalPolicy
import com.unpeel.controller.CursorMotion
import com.unpeel.controller.TerminalInput
import kotlin.math.abs
import kotlin.math.min
import kotlin.math.roundToInt

/**
 * Draws a Host terminal and takes the keyboard. Committed characters go
 * straight to the session. This view never changes the PTY size. A pinch
 * magnifies that same grid. A drag that starts on the cursor walks it
 * with arrow keys; the remote program is what actually moves it.
 */
@SuppressLint("ViewConstructor", "ClickableViewAccessibility")
class HostTerminalView(
    context: Context,
    private var terminal: HostTerminal,
) : View(context) {
    private val renderer: com.termit.ui.TerminalRenderer
    val cellWidth: Float
    val cellHeight: Int
    val topPad: Int

    var onSelectionChanged: ((Boolean) -> Unit)? = null
    /** Rising and falling edges of an on-screen select menu. */
    var onMenuChanged: ((Boolean) -> Unit)? = null
    /** Claude, Grok, or OpenCode, for the wheel guess before modes arrive. */
    var prefersRemoteWheel: Boolean = false
    /** Link relay. iOS shortens the release glide on that path. */
    var relayed: Boolean = false
    var canvasColor: Int = CANVAS
        set(value) {
            if (field == value) return
            field = value
            setBackgroundColor(value)
            invalidate()
        }
    /** Raw terminal bytes. Enter is already `\r`. A multi-line paste is bracketed and not submitted. */
    var onText: ((String) -> Unit)? = null
    /** Sticky control from the key row. Read when a letter is committed. */
    var controlArmed: () -> Boolean = { false }
    var onControlUsed: () -> Unit = {}

    private var drawn: VtEmulator? = null
    private var topRow = 0
    /** Rows above the bottom of the view. 0 keeps the cursor row on screen. */
    private var offsetRows = 0
    private var panX = 0
    /** Finger travel not yet worth a whole row, in GestureDetector distanceY sign. */
    private var scrollRemainder = 0f
    /** Wheel travel for a full-screen program. Never moves the canvas. */
    private var wheelRemainder = 0f
    /** iOS `startWheelMomentum`: steps per second after the finger lifts. */
    private var glideRate = 0f
    private var glideCarry = 0f
    private var glideX = 0f
    private var glideY = 0f
    private val glideTick = object : Runnable {
        override fun run() {
            val emulator = terminal.emulator
            val mode = forwarding(emulator)
            if (glideRate == 0f ||
                (mode != AgentTerminalPolicy.Wheel.Mouse && mode != AgentTerminalPolicy.Wheel.AlternateScroll)
            ) {
                stopGlide()
                return
            }
            glideCarry += glideRate * GLIDE_TICK_SECONDS
            val steps = abs(glideCarry).toInt()
            if (steps > 0) {
                val sign = if (glideCarry < 0f) -1 else 1
                glideCarry -= steps * sign
                val burst = min(steps, if (relayed) GLIDE_BURST_RELAY else WHEEL_BURST_CAP)
                dispatchRemoteWheel(emulator, mode, glideX, glideY, glideRate > 0f, burst)
            }
            glideRate *= if (relayed) GLIDE_FRICTION_RELAY else GLIDE_FRICTION_LAN
            if (abs(glideRate) <= GLIDE_STOP_STEPS_PER_SECOND) {
                stopGlide()
                return
            }
            postDelayed(this, GLIDE_TICK_MS)
        }
    }
    private var flingVelocity = 0f
    private var selecting = false
    private var selX1 = -1
    private var selY1 = -1
    private var selX2 = -1
    private var selY2 = -1
    private var blinkOn = true
    private var menuActive = false
    /** Visual magnification. The PTY grid stays the phone-fit size. */
    private var zoom = 1f
    private var zoomPanX = 0f
    private var zoomPanY = 0f
    /**
     * Another phone owns the Host letterbox. Scale this view so that shared
     * grid fills our width — no resize-desktop, just local fit.
     */
    var fitWidthToSharedGrid: Boolean = false
        set(value) {
            if (field == value) return
            field = value
            zoom = 1f
            zoomPanX = 0f
            zoomPanY = 0f
            if (value && width > 0 && height > 0) {
                val scale = widthFitScale(terminal.emulator)
                val scaledH = contentHeightPx(terminal.emulator) * scale
                if (scaledH > height) zoomPanY = height - scaledH
            }
            invalidate()
        }
    private var pinching = false
    private var downX = 0f
    private var downY = 0f
    private var downOnCursor = false
    private var gestureDecided = false
    private var cursorDrag = false
    private var placedCol = 0
    private var placedRow = 0
    private val touchSlop = ViewConfiguration.get(context).scaledTouchSlop.toFloat()

    /** Last coherent present — kept on screen while VT mid-repaints (anti-mosaic). */
    private var lastGoodBitmap: Bitmap? = null
    private var freezeActive = false
    /** 0 = show only [lastGoodBitmap]; 1 = fully live. Grows bottom→top while revealing. */
    private var revealProgress = 1f
    private val bitmapPaint = Paint(Paint.FILTER_BITMAP_FLAG)

    private val frameListener: () -> Unit = {
        noteEmulator(terminal.emulator)
        clampOffset(terminal.emulator)
        publishMenu()
        postInvalidateOnAnimation()
    }

    private val flingTick = object : Runnable {
        override fun run() {
            val emulator = terminal.emulator
            if (flingVelocity == 0f || forwarding(emulator) != AgentTerminalPolicy.Wheel.None) {
                flingVelocity = 0f
                return
            }
            scrollRemainder += flingVelocity * 0.016f
            applyScrollLines(emulator)
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
            stopGlide()
            wheelRemainder = 0f
            downX = e.x
            downY = e.y
            downOnCursor = tapTargetsCursor(terminal.emulator, e)
            gestureDecided = false
            cursorDrag = false
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
            performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            if (!selecting || !selectionContains(e)) selectWord(e)
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
            if (!gestureDecided) {
                val dx = e2.x - downX
                val dy = e2.y - downY
                if (abs(dx) < touchSlop && abs(dy) < touchSlop) return true
                gestureDecided = true
                cursorDrag = wantsCursorDrag(emulator, dx, dy)
                if (cursorDrag) {
                    placedCol = emulator.cursorCol
                    placedRow = emulator.cursorRow
                }
            }
            if (cursorDrag) {
                moveCursor(emulator, e2)
                return true
            }
            // Magnified, or shared-grid taller than this phone after width-fit.
            if (zoom > 1f || sharedGridOverflows()) {
                zoomPanX -= distanceX
                zoomPanY -= distanceY
                clampZoom()
                invalidate()
                return true
            }
            // Finger down (negative distance) moves toward older rows.
            // The picture shifts with the finger immediately; a whole row
            // is committed only after the finger has travelled one cell.
            if (forwardRemote(emulator, e2, distanceY)) return true
            if (distanceY != 0f && cellHeight > 0) {
                scrollRemainder += distanceY
                applyScrollLines(emulator)
            }
            panX -= distanceX.toInt()
            horizontalShift(emulator)
            invalidate()
            return true
        }

        override fun onFling(e1: MotionEvent?, e2: MotionEvent, velocityX: Float, velocityY: Float): Boolean {
            if (selecting || cursorDrag || zoom > 1f || cellHeight <= 0) return false
            val emulator = terminal.emulator
            // velocityY is downward. A positive line count reveals newer rows.
            when (forwarding(emulator)) {
                AgentTerminalPolicy.Wheel.Mouse, AgentTerminalPolicy.Wheel.AlternateScroll -> {
                    startGlide(-velocityY / cellHeight, e2.x, e2.y)
                }
                AgentTerminalPolicy.Wheel.None -> {
                    flingVelocity = -velocityY
                    postOnAnimation(flingTick)
                }
            }
            return true
        }
    })

    init {
        val textPx = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 16f, resources.displayMetrics)
            .roundToInt()
            .coerceAtLeast(8)
        val metrics = TerminalMetrics(context, textPx)
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
        val switching = this.terminal !== terminal
        if (switching && this.terminal.onFrame === frameListener) {
            this.terminal.parkedScroll = offsetRows
            this.terminal.onFrame = null
            this.terminal.onHoldFrame = null
            this.terminal.onRevealProgress = null
        }
        this.terminal = terminal
        terminal.onFrame = frameListener
        terminal.onHoldFrame = {
            // Keep the last coherent pixels; VT may already be wiping.
            freezeActive = true
            revealProgress = 0f
            postInvalidateOnAnimation()
        }
        terminal.onRevealProgress = { progress ->
            // Host snaps 0 ↔ 1 only (no multi-frame reverse wipe).
            revealProgress = if (progress >= 1f) 1f else 0f
            if (revealProgress >= 1f) freezeActive = false
            postInvalidateOnAnimation()
        }
        terminal.updateCellSize(cellWidth.roundToInt(), cellHeight)
        if (!switching) return
        noteEmulator(terminal.emulator)
        offsetRows = terminal.parkedScroll.coerceAtLeast(0)
        clampOffset(terminal.emulator)
        freezeActive = false
        revealProgress = 1f
        postInvalidateOnAnimation()
    }

    fun selectedText(): String {
        val args = selectionArgs() ?: return ""
        return runCatching {
            terminal.emulator.getSelectedText(args[2], args[0], args[3], args[1])
        }.getOrDefault("")
    }

    /** Copies the selection onto the system clipboard. */
    fun copySelection(clear: Boolean = true): Boolean {
        val text = selectedText()
        if (text.isEmpty()) return false
        val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return false
        clipboard.setPrimaryClip(ClipData.newPlainText("unpeel", text))
        if (clear) clearSelection()
        return true
    }

    /** Up, down, enter, or escape for an on-screen select menu. Arrows follow DECCKM. */
    fun menuKey(name: String): String {
        val application = terminal.emulator.isCursorKeysApplicationMode()
        return when (name) {
            "up" -> if (application) "\u001bOA" else "\u001b[A"
            "down" -> if (application) "\u001bOB" else "\u001b[B"
            "enter" -> "\r"
            else -> "\u001b"
        }
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
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or
            InputType.TYPE_TEXT_FLAG_MULTI_LINE or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
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
        stopGlide()
        if (terminal.onFrame === frameListener) terminal.onFrame = null
        super.onDetachedFromWindow()
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
            parent?.requestDisallowInterceptTouchEvent(true)
            pinching = false
        }
        scaler.onTouchEvent(event)
        if (event.pointerCount > 1 || scaler.isInProgress) {
            if (!pinching) {
                pinching = true
                cursorDrag = false
                val cancel = MotionEvent.obtain(event)
                cancel.action = MotionEvent.ACTION_CANCEL
                gesture.onTouchEvent(cancel)
                cancel.recycle()
            }
            return true
        }
        if (pinching) {
            if (event.actionMasked == MotionEvent.ACTION_UP || event.actionMasked == MotionEvent.ACTION_CANCEL) {
                pinching = false
            }
            return true
        }
        gesture.onTouchEvent(event)
        // GestureDetector does not deliver a drag that starts after the
        // long-press timeout. Extend the selection here so it can cross lines.
        if (selecting && event.actionMasked == MotionEvent.ACTION_MOVE) {
            val cell = cellAt(event.x, event.y)
            if (cell != null) {
                selX2 = cell.first
                selY2 = cell.second
                invalidate()
            }
        }
        return true
    }

    override fun onDraw(canvas: Canvas) {
        val emulator = terminal.emulator
        noteEmulator(emulator)
        val held = lastGoodBitmap
        if (freezeActive && revealProgress < 1f && held != null && !held.isRecycled) {
            // Hold last coherent present through Cursor ED2 / sync floods.
            canvas.drawBitmap(held, 0f, 0f, bitmapPaint)
            return
        }
        // Live path: render into lastGood, then blit — next hold already has frame 0.
        val bmp = ensureLastGood()
        if (bmp != null) {
            renderLiveInto(Canvas(bmp), emulator)
            canvas.drawBitmap(bmp, 0f, 0f, bitmapPaint)
        } else {
            renderLiveInto(canvas, emulator)
        }
    }

    private fun ensureLastGood(): Bitmap? {
        if (width <= 0 || height <= 0) return null
        var bmp = lastGoodBitmap
        if (bmp == null || bmp.width != width || bmp.height != height || bmp.isRecycled) {
            bmp?.recycle()
            bmp = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            lastGoodBitmap = bmp
        }
        return bmp
    }

    private fun renderLiveInto(canvas: Canvas, emulator: VtEmulator) {
        canvas.drawColor(canvasColor)
        clampZoom()
        val scale = displayScale(emulator)
        canvas.save()
        canvas.translate(zoomPanX, zoomPanY)
        canvas.scale(scale, scale)
        canvas.save()
        val placed = place(emulator)
        topRow = placed.first
        val shift = if (forwarding(emulator) != AgentTerminalPolicy.Wheel.None) {
            panX = 0
            0
        } else if (fitWidthToSharedGrid) {
            // Width-fit already fills the view; don't also pan the grid.
            panX = 0
            0
        } else {
            horizontalShift(emulator)
        }
        // Follower vertical pin is zoomPanY; skip place()'s bottom-pin translate.
        val y = if (fitWidthToSharedGrid) {
            if (placed.first < 0 || offsetRows != 0) placed.second else 0
        } else {
            placed.second
        }
        canvas.translate(shift.toFloat(), y.toFloat())
        val sel = selectionArgs()
        if (sel == null) {
            renderer.render(emulator, canvas, topRow, -1, -1, -1, -1, blinkOn)
        } else {
            renderer.render(emulator, canvas, topRow, sel[0], sel[1], sel[2], sel[3], blinkOn)
        }
        canvas.restore()
        canvas.restore()
    }

    /** Pinch zoom on top of optional shared-grid width fit. */
    private fun displayScale(emulator: VtEmulator): Float = widthFitScale(emulator) * zoom

    private fun widthFitScale(emulator: VtEmulator): Float {
        if (!fitWidthToSharedGrid || width <= 0 || cellWidth <= 0f) return 1f
        val contentW = emulator.mColumns * cellWidth
        if (contentW <= 1f) return 1f
        return (width / contentW).coerceIn(0.55f, 1.85f)
    }

    private fun contentHeightPx(emulator: VtEmulator): Float =
        topPad + emulator.mRows * cellHeight.toFloat()

    private fun sharedGridOverflows(): Boolean {
        if (!fitWidthToSharedGrid || height <= 0) return false
        return contentHeightPx(terminal.emulator) * displayScale(terminal.emulator) > height + 0.5f
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        if (!fitWidthToSharedGrid || w <= 0 || h <= 0) return
        zoom = 1f
        zoomPanX = 0f
        val scale = widthFitScale(terminal.emulator)
        val scaledH = contentHeightPx(terminal.emulator) * scale
        zoomPanY = if (scaledH > h) h - scaledH else 0f
    }

    private fun publishMenu() {
        val emulator = terminal.emulator
        val text = runCatching {
            emulator.getSelectedText(0, 0, emulator.mColumns, emulator.mRows)
        }.getOrDefault("")
        val active = AgentTerminalPolicy.viewportHasMenuPrompt(text)
        if (active == menuActive) return
        menuActive = active
        onMenuChanged?.invoke(active)
    }

    private fun noteEmulator(emulator: VtEmulator) {
        if (drawn === emulator) return
        drawn = emulator
        topRow = 0
        offsetRows = 0
        panX = 0
        scrollRemainder = 0f
        wheelRemainder = 0f
        stopGlide()
        flingVelocity = 0f
        if (selecting) clearSelection()
        if (menuActive) {
            menuActive = false
            onMenuChanged?.invoke(false)
        }
    }

    /** Turns accumulated finger travel into whole rows of the local scrollback. */
    private fun applyScrollLines(emulator: VtEmulator) {
        if (cellHeight <= 0) return
        val lines = (scrollRemainder / cellHeight).toInt()
        if (lines == 0) return
        val applied = if (lines > 0) lines else -kotlin.math.abs(lines)
        scrollRemainder -= applied * cellHeight
        offsetRows = (offsetRows - applied).coerceIn(0, maxOffset(emulator))
        clampOffset(emulator)
    }

    /** iOS `wheelForwarding`. The view stays put for anything other than local scrollback. */
    private fun forwarding(emulator: VtEmulator): AgentTerminalPolicy.Wheel {
        return AgentTerminalPolicy.wheelForwarding(
            modesKnown = terminal.hostModeSnapshot,
            mouseTracking = emulator.isMouseTrackingActive(),
            alternateScreen = emulator.isAlternateBufferActive(),
            sawMouseOrAlternateDisable = terminal.sawMouseOrAlternateDisable,
            prefersRemoteMouseWheel = prefersRemoteWheel,
        )
    }

    /** True when the gesture was consumed by the remote program. */
    private fun forwardRemote(emulator: VtEmulator, event: MotionEvent, distanceY: Float): Boolean {
        val mode = forwarding(emulator)
        if (mode == AgentTerminalPolicy.Wheel.None) return false
        panX = 0
        if (distanceY == 0f || cellHeight <= 0) return true
        wheelRemainder += distanceY
        val lines = (wheelRemainder / cellHeight).toInt()
        if (lines == 0) return true
        val steps = min(abs(lines), WHEEL_BURST_CAP)
        val applied = if (lines > 0) steps else -steps
        wheelRemainder -= applied * cellHeight
        dispatchRemoteWheel(emulator, mode, event.x, event.y, applied > 0, steps)
        return true
    }

    /**
     * iOS `startWheelMomentum`. A slow lift is a placed drag and sends nothing
     * more. A flick decays into wheel bursts until friction wins. The picture
     * stays put; Claude still repaints each burst.
     */
    private fun startGlide(stepsPerSecond: Float, x: Float, y: Float) {
        if (abs(stepsPerSecond) <= GLIDE_START_STEPS_PER_SECOND) return
        val cap = if (relayed) GLIDE_CAP_RELAY else GLIDE_CAP_LAN
        glideRate = stepsPerSecond.coerceIn(-cap, cap)
        glideCarry = 0f
        glideX = x
        glideY = y
        removeCallbacks(glideTick)
        post(glideTick)
    }

    private fun stopGlide() {
        glideRate = 0f
        glideCarry = 0f
        removeCallbacks(glideTick)
    }

    private fun dispatchRemoteWheel(
        emulator: VtEmulator,
        mode: AgentTerminalPolicy.Wheel,
        x: Float,
        y: Float,
        towardBottom: Boolean,
        steps: Int,
    ) {
        if (steps <= 0) return
        when (mode) {
            AgentTerminalPolicy.Wheel.Mouse -> sendWheel(emulator, x, y, towardBottom, steps)
            AgentTerminalPolicy.Wheel.AlternateScroll -> sendAlternateScroll(towardBottom, steps)
            AgentTerminalPolicy.Wheel.None -> Unit
        }
    }

    /** Cursor up/down for a full-screen program that does not track the mouse. */
    private fun sendAlternateScroll(towardBottom: Boolean, steps: Int) {
        val application = terminal.emulator.isCursorKeysApplicationMode()
        val key = if (towardBottom) {
            if (application) "\u001bOB" else "\u001b[B"
        } else {
            if (application) "\u001bOA" else "\u001b[A"
        }
        onText?.invoke(key.repeat(steps.coerceAtLeast(1)))
    }

    /** Rows of the current screen that sit above the view, plus saved scrollback. */
    private fun maxOffset(emulator: VtEmulator): Int {
        val history = if (emulator.alternateBufferActive) 0 else emulator.screen.activeTranscriptRows
        val hidden = (emulator.mRows - visibleRows()).coerceAtLeast(0)
        return hidden + history
    }

    private fun visibleRows(): Int {
        if (cellHeight <= 0) return 1
        return ((height - topPad) / cellHeight).coerceAtLeast(1)
    }

    private fun clampOffset(emulator: VtEmulator) {
        offsetRows = offsetRows.coerceIn(0, maxOffset(emulator))
    }

    /** topRow for the renderer, and the Y translation that puts that row in view. */
    private fun place(emulator: VtEmulator): Pair<Int, Int> {
        if (forwarding(emulator) != AgentTerminalPolicy.Wheel.None) return 0 to minPanY(emulator)
        clampOffset(emulator)
        if (offsetRows == 0 && scrollRemainder > 0f) scrollRemainder = 0f
        if (offsetRows == maxOffset(emulator) && scrollRemainder < 0f) scrollRemainder = 0f
        val hidden = (emulator.mRows - visibleRows()).coerceAtLeast(0)
        val intoHistory = (offsetRows - hidden).coerceAtLeast(0)
        val row = -intoHistory
        val base = if (intoHistory > 0) 0 else minPanY(emulator) + offsetRows * cellHeight
        // Negative remainder is a finger moving down, so the picture moves down with it.
        val slack = Math.round(-scrollRemainder)
        return row to (base + slack)
    }

    /** Left press then release at the tapped cell. Column and row are 1-based. */
    private fun sendClick(emulator: VtEmulator, event: MotionEvent) {
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

    private val scaler = ScaleGestureDetector(context, object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
        override fun onScale(detector: ScaleGestureDetector): Boolean {
            val emulator = terminal.emulator
            val fit = widthFitScale(emulator)
            val oldScale = fit * zoom
            if (oldScale <= 0f) return false
            val nextZoom = (zoom * detector.scaleFactor).coerceIn(1f, MAX_ZOOM)
            val focusX = detector.focusX
            val focusY = detector.focusY
            val contentX = (focusX - zoomPanX) / oldScale
            val contentY = (focusY - zoomPanY) / oldScale
            zoom = nextZoom
            val newScale = fit * zoom
            zoomPanX = focusX - contentX * newScale
            zoomPanY = focusY - contentY * newScale
            clampZoom()
            invalidate()
            return true
        }
    }).also { it.isQuickScaleEnabled = false }

    /** A magnified picture stays inside the view. At 1× the pan is zero. */
    private fun clampZoom() {
        if (width <= 0 || height <= 0) return
        val emulator = terminal.emulator
        if (!fitWidthToSharedGrid) {
            if (zoom <= 1f) {
                zoom = 1f
                zoomPanX = 0f
                zoomPanY = 0f
                return
            }
            val minX = -(zoom - 1f) * width
            val minY = -(zoom - 1f) * height
            zoomPanX = zoomPanX.coerceIn(minX, 0f)
            zoomPanY = zoomPanY.coerceIn(minY, 0f)
            return
        }
        if (zoom < 1f) zoom = 1f
        zoomPanX = 0f
        val scale = displayScale(emulator)
        val scaledH = contentHeightPx(emulator) * scale
        if (scaledH <= height) {
            zoomPanY = 0f
        } else {
            zoomPanY = zoomPanY.coerceIn(height - scaledH, 0f)
        }
    }

    /**
     * Drag that starts on the cursor. A shell only follows the finger along
     * that row. A full-screen program follows it in both directions.
     * Mouse-tracking programs keep the pointer.
     */
    private fun wantsCursorDrag(emulator: VtEmulator, dx: Float, dy: Float): Boolean {
        if (!downOnCursor || selecting || offsetRows != 0 || emulator.isMouseTrackingActive()) return false
        if (emulator.isAlternateBufferActive()) return true
        return abs(dx) > abs(dy)
    }

    private fun moveCursor(emulator: VtEmulator, event: MotionEvent) {
        val cell = cellAt(event.x, event.y) ?: return
        val step = CursorMotion.step(
            fromCol = placedCol,
            fromRow = placedRow,
            toCol = cell.first,
            toRow = cell.second,
            allowRows = emulator.isAlternateBufferActive(),
            applicationCursor = emulator.isCursorKeysApplicationMode(),
        )
        if (step.keys.isEmpty()) return
        placedCol = step.col
        placedRow = step.row
        onText?.invoke(step.keys)
    }

    private fun tapTargetsCursor(emulator: VtEmulator, event: MotionEvent): Boolean {
        val cell = cellAt(event.x, event.y) ?: return false
        return kotlin.math.abs(cell.second - emulator.cursorRow) <= 1
    }

    private fun sendWheel(emulator: VtEmulator, x: Float, y: Float, towardBottom: Boolean, steps: Int) {
        val cell = cellAt(x, y)
        val column = ((cell?.first ?: 0) + 1).coerceIn(1, emulator.mColumns)
        val row = ((cell?.second ?: 0) + 1).coerceIn(1, emulator.mRows)
        val button = if (towardBottom) VtEmulator.MOUSE_WHEELDOWN_BUTTON else VtEmulator.MOUSE_WHEELUP_BUTTON
        // iOS always sends SGR for a guessed wheel, before the program has enabled tracking.
        val sgr = emulator.isMouseProtocolSgr() || !emulator.isMouseTrackingActive()
        val payload = if (sgr) {
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
    private fun minPanY(emulator: VtEmulator): Int {
        // Width-fit follower pins via zoomPanY instead.
        if (fitWidthToSharedGrid) return 0
        val contentHeight = topPad + emulator.mRows * cellHeight
        return min(0, height - contentHeight)
    }

    private fun horizontalShift(emulator: VtEmulator): Int {
        val contentWidth = (emulator.mColumns * cellWidth).roundToInt()
        val minPan = min(0, width - contentWidth)
        if (panX < minPan) panX = minPan
        if (panX > 0) panX = 0
        return panX
    }

    private fun cellAt(x: Float, y: Float): Pair<Int, Int>? {
        val emulator = terminal.emulator
        val placed = place(emulator)
        val scale = displayScale(emulator)
        if (scale <= 0f) return null
        val viewX = (x - zoomPanX) / scale
        val viewY = (y - zoomPanY) / scale
        val shift = if (fitWidthToSharedGrid) 0 else horizontalShift(emulator)
        val yOff = if (fitWidthToSharedGrid) {
            if (placed.first < 0 || offsetRows != 0) placed.second else 0
        } else {
            placed.second
        }
        val contentX = viewX - shift
        val contentY = viewY - yOff
        val localRow = ((contentY - topPad) / cellHeight).toInt()
        if (localRow < 0 || localRow >= emulator.mRows) return null
        val col = (contentX / cellWidth).toInt()
        if (col < 0 || col >= emulator.mColumns) return null
        return col to (placed.first + localRow)
    }

    /** Selects the word under the finger, or the whole line when the cell is blank. */
    private fun selectWord(event: MotionEvent) {
        val cell = cellAt(event.x, event.y) ?: return
        val row = cell.second
        val cols = terminal.emulator.mColumns
        val col = cell.first.coerceIn(0, cols - 1)
        fun occupied(column: Int): Boolean {
            val text = terminal.emulator.getSelectedText(column, row, column, row)
            return text.isNotEmpty() && !text[0].isWhitespace()
        }
        var start = col
        var end = col
        if (occupied(col)) {
            while (start > 0 && occupied(start - 1)) start--
            while (end < cols - 1 && occupied(end + 1)) end++
        } else {
            start = 0
            end = cols - 1
        }
        selX1 = start
        selY1 = row
        selX2 = end
        selY2 = row
        if (!selecting) {
            selecting = true
            onSelectionChanged?.invoke(true)
        }
        invalidate()
    }

    private fun selectionContains(event: MotionEvent): Boolean {
        val args = selectionArgs() ?: return false
        val cell = cellAt(event.x, event.y) ?: return false
        val row = cell.second
        val col = cell.first
        if (row < args[0] || row > args[1]) return false
        val startCol = if (row == args[0]) args[2] else 0
        val endCol = if (row == args[1]) args[3] else terminal.emulator.mColumns - 1
        return col in startCol..endCol
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
        /** iOS `maximumMouseWheelEventsPerGestureTick`. */
        const val WHEEL_BURST_CAP = 8
        const val GLIDE_START_STEPS_PER_SECOND = 6f
        const val GLIDE_STOP_STEPS_PER_SECOND = 3f
        const val GLIDE_CAP_LAN = 90f
        const val GLIDE_CAP_RELAY = 45f
        const val GLIDE_FRICTION_LAN = 0.92f
        const val GLIDE_FRICTION_RELAY = 0.90f
        const val GLIDE_BURST_RELAY = 5
        const val GLIDE_TICK_MS = 50L
        const val GLIDE_TICK_SECONDS = 0.05f
        const val MAX_ZOOM = 3f
    }
}

/**
 * Composing text stays here until the IME finishes it. A keyboard that
 * holds a whole English word must still deliver that word when Enter
 * arrives, instead of dropping the composition.
 */
private class TerminalInputConnection(private val terminal: HostTerminalView) : BaseInputConnection(terminal, false) {
    private var composing = ""

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val raw = text?.toString().orEmpty()
        val pending = composing
        composing = ""
        // Enter that only replaces the composition still has to send the word.
        if (pending.isNotEmpty() && raw.all { it == '\n' || it == '\r' }) terminal.emitCommitted(pending)
        if (raw.isNotEmpty()) terminal.emitCommitted(raw)
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        composing = text?.toString().orEmpty()
        return true
    }

    override fun finishComposingText(): Boolean {
        val pending = composing
        composing = ""
        if (pending.isNotEmpty()) terminal.emitCommitted(pending)
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        if (composing.isNotEmpty()) {
            val drop = beforeLength.coerceIn(0, composing.length)
            composing = composing.dropLast(drop)
            return true
        }
        val event = KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL)
        repeat(beforeLength.coerceAtLeast(0)) { terminal.handleTerminalKey(KeyEvent.KEYCODE_DEL, event) }
        return true
    }

    override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int): Boolean =
        deleteSurroundingText(beforeLength, afterLength)

    override fun sendKeyEvent(event: KeyEvent): Boolean {
        if (event.action != KeyEvent.ACTION_DOWN) return true
        if (composing.isNotEmpty() && (event.keyCode == KeyEvent.KEYCODE_DEL || event.keyCode == KeyEvent.KEYCODE_FORWARD_DEL)) {
            composing = composing.dropLast(1)
            return true
        }
        if (composing.isNotEmpty()) {
            val pending = composing
            composing = ""
            terminal.emitCommitted(pending)
        }
        terminal.handleTerminalKey(event.keyCode, event)
        return true
    }

    override fun getTextBeforeCursor(length: Int, flags: Int): CharSequence = ""

    override fun getTextAfterCursor(length: Int, flags: Int): CharSequence = ""
}
