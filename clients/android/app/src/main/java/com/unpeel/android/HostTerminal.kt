package com.unpeel.android

import android.util.Log
import com.termux.terminal.TerminalColors
import com.termux.terminal.TerminalEmulator
import com.termux.terminal.TerminalOutput
import com.termux.terminal.TerminalSession
import com.termux.terminal.TerminalSessionClient
import com.termux.terminal.TextStyle

/**
 * Remote-only Termux emulator. Bytes come from the Host journal.
 * Replies (cursor reports, device attributes) go back to that Host.
 * History replay does not answer queries that were already answered.
 */
class HostTerminal(
    private val replies: (ByteArray) -> Unit,
    private val onCopy: (String) -> Unit,
) {
    var onFrame: (() -> Unit)? = null
        internal set

    var emulator: TerminalEmulator
        private set

    val columns: Int get() = emulator.mColumns
    val rows: Int get() = emulator.mRows

    private var live = false
    private var cellWidth = 8
    private var cellHeight = 16

    private val output = object : TerminalOutput() {
        override fun write(data: ByteArray, offset: Int, count: Int) {
            if (!live || count <= 0) return
            replies(data.copyOfRange(offset, offset + count))
        }

        override fun titleChanged(oldTitle: String?, newTitle: String?) {}

        override fun onCopyTextToClipboard(text: String?) {
            if (live && !text.isNullOrEmpty()) onCopy(text)
        }

        override fun onPasteTextFromClipboard() {}

        override fun onBell() {}

        override fun onColorsChanged() {
            onFrame?.invoke()
        }
    }

    private val client = object : TerminalSessionClient {
        override fun onTextChanged(changedSession: TerminalSession?) {}
        override fun onTitleChanged(changedSession: TerminalSession?) {}
        override fun onSessionFinished(finishedSession: TerminalSession?) {}
        override fun onCopyTextToClipboard(session: TerminalSession?, text: String?) {}
        override fun onPasteTextFromClipboard(session: TerminalSession?) {}
        override fun onBell(session: TerminalSession?) {}
        override fun onColorsChanged(session: TerminalSession?) {}
        override fun onTerminalCursorStateChange(state: Boolean) {}
        override fun getTerminalCursorStyle(): Int? = null
        override fun logError(tag: String?, message: String?) { Log.e(tag, message.orEmpty()) }
        override fun logWarn(tag: String?, message: String?) { Log.w(tag, message.orEmpty()) }
        override fun logInfo(tag: String?, message: String?) { Log.i(tag, message.orEmpty()) }
        override fun logDebug(tag: String?, message: String?) { Log.d(tag, message.orEmpty()) }
        override fun logVerbose(tag: String?, message: String?) { Log.v(tag, message.orEmpty()) }
        override fun logStackTraceWithMessage(tag: String?, message: String?, e: Exception?) {
            Log.e(tag, message.orEmpty(), e)
        }
        override fun logStackTrace(tag: String?, e: Exception?) { Log.e(tag, "terminal", e) }
    }

    init {
        // Default cells are not painted; the view shows this color through.
        // Keep the palette in agreement so reverse video matches.
        TerminalColors.COLOR_SCHEME.mDefaultColors[TextStyle.COLOR_INDEX_BACKGROUND] = CANVAS
        emulator = create(80, 24)
    }

    /** Drop the screen and start a new grid. Does not talk to the Host. */
    fun prepare(columns: Int, rows: Int) {
        live = false
        emulator = create(columns.coerceIn(2, 300), rows.coerceIn(2, 120))
        onFrame?.invoke()
    }

    fun feed(bytes: ByteArray, live: Boolean) {
        if (bytes.isEmpty()) return
        this.live = live
        emulator.append(bytes, bytes.size)
        onFrame?.invoke()
    }

    fun updateCellSize(width: Int, height: Int) {
        if (width <= 0 || height <= 0) return
        cellWidth = width
        cellHeight = height
        emulator.resize(emulator.mColumns, emulator.mRows, width, height)
    }

    private fun create(columns: Int, rows: Int): TerminalEmulator {
        return TerminalEmulator(output, columns, rows, cellWidth, cellHeight, TRANSCRIPT_ROWS, client).also {
            it.setCursorBlinkingEnabled(true)
            it.setCursorBlinkState(true)
        }
    }

    private companion object {
        const val TRANSCRIPT_ROWS = 2000
        const val CANVAS = 0xFF1A1B1D.toInt()
    }
}
