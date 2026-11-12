package com.unpeel.android

import com.termit.vt.VtEmulator
import com.unpeel.controller.TerminalModeSignals

/**
 * Remote-only terminal. Bytes come from the Host journal.
 * Display VT is termit's [VtEmulator] (replaces the old Termux emulator).
 */
class HostTerminal(
    @Suppress("UNUSED_PARAMETER") private val replies: (ByteArray) -> Unit,
    @Suppress("UNUSED_PARAMETER") private val onCopy: (String) -> Unit,
) {
    var onFrame: (() -> Unit)? = null
        internal set

    var emulator: VtEmulator
        private set

    val columns: Int get() = emulator.mColumns
    val rows: Int get() = emulator.mRows

    private var live = false
    private var appliedCanvas = CANVAS

    private val modes = TerminalModeSignals()

    /** True after a host mode preamble was fed. Output alone is not a snapshot. */
    val hostModeSnapshot: Boolean get() = modes.hostModeSnapshot

    /** The fed bytes turned mouse tracking or the alternate screen off, or reset the terminal. */
    val sawMouseOrAlternateDisable: Boolean get() = modes.sawMouseOrAlternateDisable

    /** Scrollback rows the view had parked when another session took the screen. */
    var parkedScroll: Int = 0

    init {
        emulator = VtEmulator(80, 24, TRANSCRIPT_ROWS).also {
            it.setDefaultBackground(CANVAS)
        }
    }

    /**
     * Drop the screen and start a new grid. Does not talk to the Host.
     * A path switch passes notify false, then feeds the replacement in the
     * same turn, so the view keeps the last frame until that paint.
     */
    fun prepare(columns: Int, rows: Int, notify: Boolean = true) {
        live = false
        modes.reset()
        emulator = VtEmulator(columns.coerceIn(2, 300), rows.coerceIn(2, 120), TRANSCRIPT_ROWS).also {
            it.setDefaultBackground(appliedCanvas)
        }
        if (notify) onFrame?.invoke()
    }

    /** Terminal frame color. OpenCode and Grok send a hex; everyone else stays on the default. */
    fun applyCanvas(argb: Int) {
        if (argb == appliedCanvas) return
        appliedCanvas = argb
        emulator.setDefaultBackground(argb)
    }

    /**
     * Reflow the current screen to a new grid. Does not replay the journal,
     * so a row change cannot paint a second copy of the same frame.
     */
    fun resizeGrid(columns: Int, rows: Int) {
        val cols = columns.coerceIn(2, 300)
        val lines = rows.coerceIn(2, 120)
        if (cols == emulator.mColumns && lines == emulator.mRows) return
        emulator.resize(cols, lines)
        onFrame?.invoke()
    }

    fun feed(bytes: ByteArray, live: Boolean, notify: Boolean = true, hostSnapshot: Boolean = false) {
        if (bytes.isEmpty()) return
        if (hostSnapshot) modes.markHostSnapshot()
        modes.feed(bytes)
        this.live = live
        // termit's lean VT does not currently emit DA / cursor-report replies.
        emulator.append(bytes, bytes.size)
        if (notify) onFrame?.invoke()
    }

    fun updateCellSize(width: Int, height: Int) {
        // Pixel cell size is owned by the view; VtEmulator is cell-grid only.
        if (width <= 0 || height <= 0) return
    }

    companion object {
        private const val TRANSCRIPT_ROWS = 2000
        const val CANVAS = 0xFF1A1B1D.toInt()
    }
}
