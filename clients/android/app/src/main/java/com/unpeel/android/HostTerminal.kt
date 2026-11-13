package com.unpeel.android

import android.os.Handler
import android.os.Looper
import android.view.Choreographer
import com.termit.vt.VtEmulator
import com.unpeel.controller.TerminalModeSignals

/**
 * Remote-only terminal. Bytes come from the Host journal.
 *
 * Present policy while a TUI redraws hard:
 * 1. VT always consumes every byte.
 * 2. Full wipe / alt-screen / DEC 2026 → freeze the last good bitmap.
 * 3. Keep freezing through the flood; debounce a quiet window.
 * 4. When quiet, snap once to live — no multi-frame reverse wipe.
 *
 * Claude / Grok / OpenCode ([holdLiveRepaints] = false): live ED2 already
 * rebuilds inside one append — paint immediately so wheel scroll stays fluid.
 *
 * Cursor-style local-scroll agents ([holdLiveRepaints] = true): a tiny style
 * change often ED2/DEC2026-repaints the whole screen; keep the settle freeze
 * so the phone does not flash every intermediate wipe.
 */
class HostTerminal(
    @Suppress("UNUSED_PARAMETER") private val replies: (ByteArray) -> Unit,
    @Suppress("UNUSED_PARAMETER") private val onCopy: (String) -> Unit,
) {
    var onFrame: (() -> Unit)? = null
        internal set

    /**
     * View hooks: [onHoldFrame] keeps the last coherent pixels;
     * [onRevealProgress] is 0 while frozen and 1 when live again.
     */
    var onHoldFrame: (() -> Unit)? = null
    var onRevealProgress: ((Float) -> Unit)? = null

    /**
     * Cursor / local-scroll agents: hold through live ED2 and DEC 2026.
     * Claude / Grok / OpenCode leave this false for immediate live paint.
     */
    var holdLiveRepaints: Boolean = false

    var emulator: VtEmulator
        private set

    val columns: Int get() = emulator.mColumns
    val rows: Int get() = emulator.mRows

    /** True while the view should keep the frozen bitmap (not live VT). */
    val holdingFrame: Boolean get() = holdArmed

    private var live = false
    private var appliedCanvas = CANVAS

    private val modes = TerminalModeSignals()

    val hostModeSnapshot: Boolean get() = modes.hostModeSnapshot
    val sawMouseOrAlternateDisable: Boolean get() = modes.sawMouseOrAlternateDisable
    var parkedScroll: Int = 0

    private val paintHandler = Handler(Looper.getMainLooper())
    private var paintDirty = false
    private var frameScheduled = false
    private var holdArmed = false
    private var settleGeneration = 0

    private val frameCallback = Choreographer.FrameCallback {
        frameScheduled = false
        if (emulator.synchronizedOutput) return@FrameCallback
        // Frozen until settle snaps us live.
        if (holdArmed) return@FrameCallback
        if (!paintDirty) return@FrameCallback
        paintDirty = false
        onFrame?.invoke()
        if (paintDirty) scheduleFrame()
    }

    private val settleRunnable = Runnable {
        if (!holdArmed) return@Runnable
        if (emulator.synchronizedOutput) {
            // Still inside ?2026h — retry; do not drop the settle chain
            // or a missing end callback freezes the bitmap forever.
            armSettle()
            return@Runnable
        }
        // One shot to live. No bottom→top scrub.
        holdArmed = false
        emulator.clearPresentHold()
        paintDirty = true
        onRevealProgress?.invoke(1f)
        scheduleFrame()
    }

    init {
        emulator = newEmulator(80, 24)
    }

    private fun paintLiveWithoutHold(): Boolean =
        live && !holdLiveRepaints && !emulator.synchronizedOutput

    private fun newEmulator(columns: Int, rows: Int): VtEmulator =
        VtEmulator(columns, rows, TRANSCRIPT_ROWS).also { emu ->
            emu.setDefaultBackground(appliedCanvas)
            emu.onPresentHold = {
                paintHandler.post {
                    // Claude/Grok live ED2 paints in feed(); skip re-arming.
                    if (live && !holdLiveRepaints && !emu.synchronizedOutput) return@post
                    beginHold()
                    armSettle()
                }
            }
            emu.onSynchronizedOutputEnd = {
                paintHandler.post {
                    paintDirty = true
                    if (paintLiveWithoutHold()) {
                        releaseHoldForLivePaint()
                        scheduleFrame()
                    } else {
                        armSettle()
                    }
                }
            }
        }

    private fun beginHold() {
        if (holdArmed) return
        holdArmed = true
        onHoldFrame?.invoke()
        onRevealProgress?.invoke(0f)
    }

    private fun releaseHoldForLivePaint() {
        if (!holdArmed && !emulator.presentHold) return
        holdArmed = false
        settleGeneration++
        paintHandler.removeCallbacks(settleRunnable)
        emulator.clearPresentHold()
        onRevealProgress?.invoke(1f)
    }

    private fun armSettle() {
        val gen = ++settleGeneration
        paintHandler.removeCallbacks(settleRunnable)
        paintHandler.postDelayed({
            if (gen != settleGeneration) return@postDelayed
            settleRunnable.run()
        }, SETTLE_MS)
    }

    /**
     * Arm the view freeze before a mid-session rebuild. Used when the phone
     * claims letterbox and the column count changes — keep last pixels up
     * instead of the prepare cover flash.
     */
    fun holdPresent() {
        beginHold()
    }

    fun prepare(columns: Int, rows: Int, notify: Boolean = true, holdPixels: Boolean = false) {
        live = false
        modes.reset()
        settleGeneration++
        paintHandler.removeCallbacks(settleRunnable)
        if (holdPixels) {
            beginHold()
        } else {
            holdArmed = false
            onRevealProgress?.invoke(1f)
        }
        emulator = newEmulator(columns.coerceIn(2, 300), rows.coerceIn(2, 120))
        if (notify) publishFrame()
    }

    fun applyCanvas(argb: Int) {
        if (argb == appliedCanvas) return
        appliedCanvas = argb
        emulator.setDefaultBackground(argb)
        // Host App color / provider theme changed — repaint, don't keep a stale freeze.
        publishFrame()
    }

    fun resizeGrid(columns: Int, rows: Int) {
        val cols = columns.coerceIn(2, 300)
        val lines = rows.coerceIn(2, 120)
        if (cols == emulator.mColumns && lines == emulator.mRows) return
        emulator.resize(cols, lines)
        publishFrame()
    }

    fun feed(bytes: ByteArray, live: Boolean, notify: Boolean = true, hostSnapshot: Boolean = false) {
        if (bytes.isEmpty()) return
        if (hostSnapshot) modes.markHostSnapshot()
        modes.feed(bytes)
        this.live = live
        emulator.append(bytes, bytes.size)
        if (!notify) return
        paintDirty = true
        if (emulator.synchronizedOutput) {
            // Mid DEC 2026 bracket — keep last good pixels until ?2026l.
            beginHold()
            armSettle()
            return
        }
        if (paintLiveWithoutHold()) {
            // Claude/Grok/OpenCode wheel redraw: show this frame now.
            releaseHoldForLivePaint()
            scheduleFrame()
            return
        }
        if (emulator.presentHold || holdArmed) {
            // Cursor live repaints + replay wipe floods: hold until quiet.
            if (emulator.presentHold) beginHold()
            armSettle()
            return
        }
        scheduleFrame()
    }

    fun publishFrame() {
        paintDirty = false
        holdArmed = false
        settleGeneration++
        paintHandler.removeCallbacks(settleRunnable)
        if (frameScheduled) {
            Choreographer.getInstance().removeFrameCallback(frameCallback)
            frameScheduled = false
        }
        emulator.clearPresentHold()
        onRevealProgress?.invoke(1f)
        val paint = onFrame
        if (paint != null && Looper.myLooper() == Looper.getMainLooper()) {
            paint.invoke()
        } else {
            paintHandler.post { onFrame?.invoke() }
        }
    }

    private fun scheduleFrame() {
        if (frameScheduled) return
        frameScheduled = true
        Choreographer.getInstance().postFrameCallback(frameCallback)
    }

    fun updateCellSize(width: Int, height: Int) {
        if (width <= 0 || height <= 0) return
    }

    companion object {
        private const val TRANSCRIPT_ROWS = 2000
        /**
         * Quiet window after a wipe/flood before showing live VT.
         * Cursor live ED2 / DEC 2026 and replay still use this debounce.
         */
        private const val SETTLE_MS = 160L
        const val CANVAS = 0xFF1A1B1D.toInt()
    }
}
