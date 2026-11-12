package com.termit.vt

/**
 * Lean VT100/ANSI subset for remote Unpeel Host streams.
 * Enough for agent TUIs: margins, insert/delete line/char, erase, OSC colors.
 */
class VtEmulator(
    cols: Int = 80,
    rows: Int = 24,
    scrollbackLimit: Int = 2000,
) {
    private val scrollbackLimit = scrollbackLimit.coerceAtLeast(0)
    private var primary = ScreenBuffer(cols, rows, this.scrollbackLimit)
    private var alternate = ScreenBuffer(cols, rows, scrollbackLimit = 0)
    private var onAlt = false

    val screen: ScreenBuffer get() = if (onAlt) alternate else primary

    var cursorRow = 0
        private set
    var cursorCol = 0
        private set

    var mouseTrackingActive = false
        private set
    var mouseProtocolSgr = false
        private set
    var bracketedPaste = false
        private set
    var alternateBufferActive = false
        private set
    var applicationCursorKeys = false
        private set
    /** DECSET 25 — when false the block cursor is hidden regardless of blink. */
    var cursorDisplayEnabled = true
        private set
    /** DECSET 7 — DECAWM. Ghostty snapshots and Cursor redraws depend on this. */
    var autoWrap = true
        private set
    /** DECSET 6 — DECOM. CUP / home are relative to the scroll region. */
    var originMode = false
        private set
    /**
     * DECSET 2026 — synchronized output. Grid still updates; the view should
     * not present until this clears (same gate Ghostty uses).
     */
    var synchronizedOutput = false
        private set
    /**
     * Set after a full wipe / alt-screen enter — presenting mid-repaint is what
     * made Termux look mosaic. The host freezes the last good frame until the
     * stream settles, then reverse-fills from the bottom.
     */
    var presentHold = false
        private set

    /** Host-bound replies (DA/DSR/cursor reports). */
    var onReply: ((ByteArray) -> Unit)? = null
    var onBell: (() -> Unit)? = null
    var onTitle: ((String) -> Unit)? = null
    var onCopy: ((String) -> Unit)? = null
    /** Fired when synchronized output ends (`?2026l`) so the host can paint once. */
    var onSynchronizedOutputEnd: (() -> Unit)? = null
    /** Fired when [presentHold] rises — host should snapshot the last good frame. */
    var onPresentHold: (() -> Unit)? = null

    private var savedCursorRow = 0
    private var savedCursorCol = 0
    private var savedStyle = CellStyle()
    private var blinkLit = true

    // --- Termux-shaped surface used by Unpeel's HostTerminal / HostTerminalView ---
    val mColumns: Int get() = screen.cols
    val mRows: Int get() = screen.rows

    fun isMouseTrackingActive(): Boolean = mouseTrackingActive
    fun isAlternateBufferActive(): Boolean = alternateBufferActive
    fun isCursorKeysApplicationMode(): Boolean = applicationCursorKeys
    fun isMouseProtocolSgr(): Boolean = mouseProtocolSgr
    fun isBracketedPasteMode(): Boolean = bracketedPaste
    fun shouldCursorBeVisible(): Boolean = cursorDisplayEnabled && blinkLit
    fun getSelectedText(x1: Int, y1: Int, x2: Int, y2: Int): String {
        if (x1 == 0 && y1 == 0 && x2 >= mColumns && y2 >= mRows) {
            val cached = fullScreenText
            if (cached != null && fullScreenTextGen == generation) return cached
            val built = selectedTextAbs(0, 0, mColumns, mRows)
            fullScreenText = built
            fullScreenTextGen = generation
            return built
        }
        return selectedTextAbs(x1, y1, x2, y2)
    }
    fun setCursorBlinkState(on: Boolean) {
        blinkLit = on
    }
    fun setCursorBlinkingEnabled(@Suppress("UNUSED_PARAMETER") on: Boolean) {}
    fun append(data: ByteArray, length: Int) = feed(data, 0, length)

    var generation: Long = 0L
        private set

    private var fullScreenText: String? = null
    private var fullScreenTextGen: Long = -1L

    companion object {
        const val MOUSE_LEFT_BUTTON = 0
        const val MOUSE_WHEELUP_BUTTON = 64
        const val MOUSE_WHEELDOWN_BUTTON = 65
    }

    private val style = CellStyle()
    private var state = State.GROUND
    private val csiParams = IntArray(16)
    private var csiParamCount = 0
    private var csiPrivate = false
    private var oscBuffer = StringBuilder()
    private var utf8Expected = 0
    private var utf8Codepoint = 0

    val colors: IntArray = com.termit.ui.AnsiColors.buildDefaultPalette()

    fun setDefaultBackground(argb: Int) {
        colors[com.termit.ui.AnsiColors.INDEX_BG] = argb or 0xff000000.toInt()
    }

    fun setDefaultForeground(argb: Int) {
        colors[com.termit.ui.AnsiColors.INDEX_FG] = argb or 0xff000000.toInt()
    }

    fun resize(cols: Int, rows: Int) {
        primary.resize(cols, rows)
        alternate.resize(cols, rows)
        cursorRow = cursorRow.coerceIn(0, screen.rows - 1)
        cursorCol = cursorCol.coerceIn(0, screen.cols - 1)
        bumpGeneration()
    }

    fun reset() {
        val c = screen.cols
        val r = screen.rows
        primary = ScreenBuffer(c, r, scrollbackLimit)
        alternate = ScreenBuffer(c, r, scrollbackLimit = 0)
        onAlt = false
        alternateBufferActive = false
        mouseTrackingActive = false
        mouseProtocolSgr = false
        bracketedPaste = false
        applicationCursorKeys = false
        cursorDisplayEnabled = true
        autoWrap = true
        originMode = false
        synchronizedOutput = false
        presentHold = false
        cursorRow = 0
        cursorCol = 0
        savedCursorRow = 0
        savedCursorCol = 0
        savedStyle = CellStyle()
        style.reset()
        state = State.GROUND
        utf8Expected = 0
        blinkLit = true
        bumpGeneration()
    }

    fun clearPresentHold() {
        presentHold = false
    }

    private fun armPresentHold() {
        if (presentHold) return
        presentHold = true
        onPresentHold?.invoke()
    }

    private fun bumpGeneration() {
        generation++
        fullScreenText = null
    }

    fun feed(data: ByteArray, offset: Int = 0, length: Int = data.size - offset) {
        if (length <= 0) return
        bumpGeneration()
        var i = offset
        val end = offset + length
        while (i < end) {
            val b = data[i].toInt() and 0xff
            i++
            when {
                utf8Expected > 0 -> {
                    if (b and 0xc0 == 0x80) {
                        utf8Codepoint = (utf8Codepoint shl 6) or (b and 0x3f)
                        utf8Expected--
                        if (utf8Expected == 0) putCodePoint(utf8Codepoint)
                    } else {
                        utf8Expected = 0
                        putCodePoint('?'.code)
                        i--
                    }
                }
                b < 0x80 -> consumeByte(b)
                b and 0xe0 == 0xc0 -> {
                    utf8Expected = 1
                    utf8Codepoint = b and 0x1f
                }
                b and 0xf0 == 0xe0 -> {
                    utf8Expected = 2
                    utf8Codepoint = b and 0x0f
                }
                b and 0xf8 == 0xf0 -> {
                    utf8Expected = 3
                    utf8Codepoint = b and 0x07
                }
                else -> putCodePoint('?'.code)
            }
        }
    }

    fun visibleText(): String {
        val sb = StringBuilder(screen.rows * (screen.cols + 1))
        for (r in 0 until screen.rows) {
            for (c in 0 until screen.cols) {
                val ch = screen.cell(r, c).char
                sb.append(if (ch == '\u0000') ' ' else ch)
            }
            if (r < screen.rows - 1) sb.append('\n')
        }
        return sb.toString()
    }

    fun selectedText(c1: Int, r1: Int, c2: Int, r2: Int, scrollOffset: Int = 0): String {
        val top = minOf(r1, r2)
        val bottom = maxOf(r1, r2)
        val left = minOf(c1, c2)
        val right = maxOf(c1, c2)
        val sb = StringBuilder()
        for (r in top..bottom) {
            val rowLeft = if (r == top) left else 0
            val rowRight = if (r == bottom) right else screen.cols - 1
            for (c in rowLeft..rowRight) {
                val ch = screen.visibleCell(r, c.coerceIn(0, screen.cols - 1), scrollOffset).char
                if (ch != '\u0000') sb.append(ch)
            }
            if (r < bottom) sb.append('\n')
        }
        return sb.toString().trimEnd()
    }

    fun selectedTextAbs(c1: Int, r1: Int, c2: Int, r2: Int): String {
        val top = minOf(r1, r2)
        val bottom = maxOf(r1, r2)
        val left = minOf(c1, c2)
        val right = maxOf(c1, c2)
        val sb = StringBuilder()
        for (r in top..bottom) {
            val rowLeft = if (r == top) left else 0
            val rowRight = if (r == bottom) right else screen.cols - 1
            for (c in rowLeft..rowRight) {
                val ch = screen.cellAtTranscript(r, c.coerceIn(0, screen.cols - 1)).char
                if (ch != '\u0000' && ch != ' ') sb.append(ch)
                else if (ch == ' ') sb.append(' ')
            }
            if (r < bottom) sb.append('\n')
        }
        return sb.toString().trimEnd()
    }

    private fun consumeByte(b: Int) {
        when (state) {
            State.GROUND -> when (b) {
                0x00 -> Unit
                0x07 -> onBell?.invoke()
                0x08 -> backspace()
                0x09 -> tab()
                0x0a, 0x0b, 0x0c -> lineFeed()
                0x0d -> carriageReturn()
                0x18, 0x1a -> {
                    // CAN / SUB — cancel sequence, stay ground
                    state = State.GROUND
                }
                0x1b -> state = State.ESCAPE
                else -> if (b >= 0x20) putCodePoint(b)
            }
            State.ESCAPE -> when (b) {
                '['.code -> {
                    csiParamCount = 0
                    csiParams[0] = 0
                    csiPrivate = false
                    state = State.CSI_PARAM
                }
                ']'.code -> {
                    oscBuffer = StringBuilder()
                    state = State.OSC
                }
                'P'.code -> state = State.DCS // swallow until ST
                // G0/G1 designate — consume the next intermediate (e.g. ESC ( B)
                '('.code, ')'.code, '*'.code, '+'.code -> state = State.ESC_IGNORE_ONE
                'c'.code -> {
                    reset()
                    state = State.GROUND
                }
                '7'.code -> {
                    saveCursor()
                    state = State.GROUND
                }
                '8'.code -> {
                    restoreCursor()
                    state = State.GROUND
                }
                'D'.code -> {
                    index()
                    state = State.GROUND
                }
                'E'.code -> {
                    carriageReturn()
                    index()
                    state = State.GROUND
                }
                'M'.code -> {
                    reverseIndex()
                    state = State.GROUND
                }
                else -> state = State.GROUND
            }
            State.ESC_IGNORE_ONE -> state = State.GROUND
            State.DCS -> when (b) {
                0x1b -> state = State.DCS_ESC
                0x07 -> state = State.GROUND
                else -> Unit
            }
            State.DCS_ESC -> state = State.GROUND // ESC \ (ST) or any final
            State.CSI_PARAM -> when (b) {
                '?'.code -> if (csiParamCount == 0 && csiParams[0] == 0) csiPrivate = true
                in '0'.code..'9'.code -> {
                    if (csiParamCount == 0) csiParamCount = 1
                    val idx = (csiParamCount - 1).coerceAtMost(csiParams.lastIndex)
                    csiParams[idx] = csiParams[idx] * 10 + (b - '0'.code)
                }
                ';'.code -> {
                    if (csiParamCount < csiParams.size) {
                        if (csiParamCount == 0) csiParamCount = 1
                        csiParams[csiParamCount] = 0
                        csiParamCount++
                    }
                }
                in 0x20..0x2f -> Unit
                in 0x40..0x7e -> {
                    execCsi(b.toChar())
                    state = State.GROUND
                }
                else -> state = State.GROUND
            }
            State.OSC -> when (b) {
                0x07 -> {
                    finishOsc()
                    state = State.GROUND
                }
                0x1b -> state = State.OSC_ESC
                else -> if (oscBuffer.length < 2048) oscBuffer.append(b.toChar())
            }
            State.OSC_ESC -> {
                if (b == '\\'.code) finishOsc()
                state = State.GROUND
            }
        }
    }

    private fun execCsi(final: Char) {
        if (csiPrivate) {
            val enabled = final == 'h'
            if (final == 'h' || final == 'l') {
                val count = if (csiParamCount == 0) 1 else csiParamCount
                for (i in 0 until count) {
                    when (csiParams[i]) {
                        1 -> applicationCursorKeys = enabled
                        6 -> {
                            originMode = enabled
                            // DECOM on: home into the region; off: absolute home.
                            cursorRow = if (enabled) screen.scrollTop else 0
                            cursorCol = 0
                        }
                        7 -> autoWrap = enabled
                        9, 1000, 1002, 1003 -> mouseTrackingActive = enabled
                        1006 -> mouseProtocolSgr = enabled
                        2004 -> bracketedPaste = enabled
                        25 -> cursorDisplayEnabled = enabled
                        47, 1047, 1049 -> setAlt(enabled)
                        2026 -> {
                            val was = synchronizedOutput
                            synchronizedOutput = enabled
                            if (enabled) armPresentHold()
                            if (was && !enabled) onSynchronizedOutputEnd?.invoke()
                        }
                    }
                }
            }
            return
        }
        when (final) {
            'A' -> cursorRow = (cursorRow - param(0, 1)).coerceAtLeast(screen.scrollTop)
            'B' -> cursorRow = (cursorRow + param(0, 1)).coerceAtMost(screen.scrollBottom)
            'C' -> cursorCol = (cursorCol + param(0, 1)).coerceAtMost(screen.cols - 1)
            'D' -> cursorCol = (cursorCol - param(0, 1)).coerceAtLeast(0)
            'H', 'f' -> {
                val row = param(0, 1) - 1
                val col = param(1, 1) - 1
                if (originMode) {
                    cursorRow = (screen.scrollTop + row).coerceIn(screen.scrollTop, screen.scrollBottom)
                    cursorCol = col.coerceIn(0, screen.cols - 1)
                } else {
                    cursorRow = row.coerceIn(0, screen.rows - 1)
                    cursorCol = col.coerceIn(0, screen.cols - 1)
                }
            }
            'J' -> eraseDisplay(param(0, 0))
            'K' -> eraseLine(param(0, 0))
            'L' -> screen.insertLines(cursorRow, param(0, 1))
            'M' -> screen.deleteLines(cursorRow, param(0, 1))
            '@' -> screen.insertChars(cursorRow, cursorCol, param(0, 1))
            'P' -> screen.deleteChars(cursorRow, cursorCol, param(0, 1))
            'X' -> screen.eraseChars(cursorRow, cursorCol, param(0, 1))
            'S' -> repeat(param(0, 1)) { screen.scrollUp() }
            'T' -> repeat(param(0, 1)) { screen.scrollDown() }
            'r' -> {
                val top = (param(0, 1) - 1).coerceIn(0, screen.rows - 1)
                val bottomParam = if (csiParamCount >= 2) csiParams[1] else 0
                val bottom = ((if (bottomParam == 0) screen.rows else bottomParam) - 1)
                    .coerceIn(top, screen.rows - 1)
                screen.setScrollRegion(top, bottom)
                cursorRow = top
                cursorCol = 0
            }
            'm' -> applySgr()
            'G' -> cursorCol = (param(0, 1) - 1).coerceIn(0, screen.cols - 1)
            'd' -> cursorRow = (param(0, 1) - 1).coerceIn(0, screen.rows - 1)
            's' -> saveCursor()
            'u' -> restoreCursor()
            'n' -> when (param(0, 0)) {
                5 -> reply("\u001b[0n")
                6 -> reply("\u001b[${cursorRow + 1};${cursorCol + 1}R")
            }
            'c' -> if (param(0, 0) == 0) reply("\u001b[?6c")
        }
    }

    private fun eraseDisplay(mode: Int) {
        when (mode) {
            0 -> screen.clearFrom(cursorRow, cursorCol)
            1 -> screen.clearAbove(cursorRow, cursorCol)
            2 -> {
                // Hold first so the view can keep the last presented pixels.
                armPresentHold()
                screen.clearAll() // xterm: do not move cursor
            }
            3 -> {
                armPresentHold()
                screen.clearAll()
                screen.clearScrollback()
            }
        }
    }

    private fun eraseLine(mode: Int) {
        when (mode) {
            0 -> screen.clearLineFrom(cursorRow, cursorCol)
            1 -> screen.clearLineTo(cursorRow, cursorCol)
            2 -> screen.clearLine(cursorRow)
        }
    }

    private fun saveCursor() {
        savedCursorRow = cursorRow
        savedCursorCol = cursorCol
        savedStyle = style.copy()
    }

    private fun restoreCursor() {
        cursorRow = savedCursorRow.coerceIn(0, screen.rows - 1)
        cursorCol = savedCursorCol.coerceIn(0, screen.cols - 1)
        style.fg = savedStyle.fg
        style.bg = savedStyle.bg
        style.bold = savedStyle.bold
        style.underline = savedStyle.underline
        style.inverse = savedStyle.inverse
        style.dim = savedStyle.dim
    }

    /** ESC D — move down one line, scroll if at bottom of region. */
    private fun index() {
        if (cursorRow >= screen.scrollBottom) screen.scrollUp() else cursorRow++
    }

    /** ESC M — move up one line, scroll down if at top of region. */
    private fun reverseIndex() {
        if (cursorRow <= screen.scrollTop) screen.scrollDown() else cursorRow--
    }

    private fun finishOsc() {
        val body = oscBuffer.toString()
        oscBuffer = StringBuilder()
        val semi = body.indexOf(';')
        val ps = if (semi < 0) body else body.substring(0, semi)
        val pt = if (semi < 0) "" else body.substring(semi + 1)
        when (ps) {
            "0", "2" -> onTitle?.invoke(pt)
            "4" -> applyOscPalette(pt)
            "10" -> parseColorSpec(pt)?.let { setDefaultForeground(it) }
            "11" -> parseColorSpec(pt)?.let { setDefaultBackground(it) }
            "12" -> parseColorSpec(pt)?.let {
                colors[com.termit.ui.AnsiColors.INDEX_CURSOR] = it or 0xff000000.toInt()
            }
            "52" -> {
                val payload = pt.substringAfter(';', pt)
                if (payload.isNotEmpty() && payload != "?") {
                    runCatching {
                        val bytes = android.util.Base64.decode(payload, android.util.Base64.DEFAULT)
                        onCopy?.invoke(bytes.toString(Charsets.UTF_8))
                    }
                }
            }
        }
    }

    /** OSC 4;c;spec[;c;spec…] */
    private fun applyOscPalette(body: String) {
        val parts = body.split(';')
        var i = 0
        while (i + 1 < parts.size) {
            val idx = parts[i].toIntOrNull() ?: break
            val spec = parts[i + 1]
            i += 2
            if (idx !in 0..255) continue
            parseColorSpec(spec)?.let { colors[idx] = it or 0xff000000.toInt() }
        }
    }

    private fun parseColorSpec(spec: String): Int? {
        val s = spec.trim()
        if (s.isEmpty() || s == "?") return null
        if (s.startsWith("#") && s.length >= 7) {
            val hex = s.substring(1, 7)
            return hex.toIntOrNull(16)?.let { 0xff000000.toInt() or it }
        }
        // rgb:rrr/ggg/bbb or rgbi:…
        if (s.startsWith("rgb:", ignoreCase = true)) {
            val channels = s.substring(4).split('/')
            if (channels.size != 3) return null
            fun ch(raw: String): Int {
                val v = raw.trim().toIntOrNull(16) ?: return 0
                return when (raw.length) {
                    1 -> (v * 0xff) / 0xf
                    2 -> v
                    else -> (v * 0xff) / 0xffff.coerceAtLeast(1)
                }.coerceIn(0, 255)
            }
            val r = ch(channels[0])
            val g = ch(channels[1])
            val b = ch(channels[2])
            return 0xff000000.toInt() or (r shl 16) or (g shl 8) or b
        }
        return null
    }

    private fun reply(text: String) {
        onReply?.invoke(text.toByteArray(Charsets.UTF_8))
    }

    private fun setAlt(enabled: Boolean) {
        if (enabled == onAlt) {
            alternateBufferActive = enabled
            return
        }
        if (enabled) {
            armPresentHold()
            saveCursor()
            onAlt = true
            alternateBufferActive = true
            alternate.clearAll()
            alternate.resetScrollRegion()
            cursorRow = 0
            cursorCol = 0
        } else {
            armPresentHold()
            onAlt = false
            alternateBufferActive = false
            restoreCursor()
        }
    }

    private fun applySgr() {
        if (csiParamCount == 0) {
            style.reset()
            return
        }
        var i = 0
        while (i < csiParamCount) {
            when (val p = csiParams[i]) {
                0 -> style.reset()
                1 -> style.bold = true
                2 -> style.dim = true
                4 -> style.underline = true
                7 -> style.inverse = true
                22 -> {
                    style.bold = false
                    style.dim = false
                }
                24 -> style.underline = false
                27 -> style.inverse = false
                in 30..37 -> style.fg = p - 30
                39 -> style.fg = CellStyle.DEFAULT_FG
                in 40..47 -> style.bg = p - 40
                49 -> style.bg = CellStyle.DEFAULT_BG
                in 90..97 -> style.fg = p - 90 + 8
                in 100..107 -> style.bg = p - 100 + 8
                38, 48 -> {
                    val isFg = p == 38
                    if (i + 1 < csiParamCount && csiParams[i + 1] == 5 && i + 2 < csiParamCount) {
                        val idx = csiParams[i + 2].coerceIn(0, 255)
                        if (isFg) style.fg = idx else style.bg = idx
                        i += 2
                    } else if (i + 1 < csiParamCount && csiParams[i + 1] == 2 && i + 4 < csiParamCount) {
                        val r = csiParams[i + 2].coerceIn(0, 255)
                        val g = csiParams[i + 3].coerceIn(0, 255)
                        val b = csiParams[i + 4].coerceIn(0, 255)
                        val argb = 0xff000000.toInt() or (r shl 16) or (g shl 8) or b
                        if (isFg) style.fg = argb else style.bg = argb
                        i += 4
                    }
                }
            }
            i++
        }
    }

    private fun param(index: Int, default: Int): Int {
        if (index >= csiParamCount) return default
        val v = csiParams[index]
        return if (v == 0 && default != 0) default else v
    }

    private fun putCodePoint(cp: Int) {
        val width = WcWidth.width(cp)
        if (width <= 0) return
        if (cursorCol + width > screen.cols) {
            if (autoWrap) {
                carriageReturn()
                lineFeed()
            } else {
                // DECAWM off: stay on this line; next write clamps into the last cells.
                cursorCol = (screen.cols - width).coerceAtLeast(0)
            }
        }
        if (cursorRow !in 0 until screen.rows || cursorCol >= screen.cols) return
        val ch = if (cp in 0..0xFFFF) cp.toChar() else '\uFFFD'
        val row = cursorRow
        val col = cursorCol

        if (col > 0 && screen.cell(row, col).char == '\u0000') {
            screen.cell(row, col - 1).reset()
        }
        if (col + 1 < screen.cols && screen.cell(row, col + 1).char == '\u0000') {
            screen.cell(row, col + 1).reset()
        }

        val cell = screen.cell(row, col)
        cell.char = ch
        cell.applyStyle(style)
        if (width == 2 && col + 1 < screen.cols) {
            val cont = screen.cell(row, col + 1)
            cont.char = '\u0000'
            cont.applyStyle(style)
        }
        cursorCol = col + width
    }

    private fun backspace() {
        if (cursorCol > screen.cols) cursorCol = screen.cols
        if (cursorCol > 0) cursorCol--
    }

    private fun tab() {
        cursorCol = ((cursorCol / 8) + 1) * 8
        if (cursorCol >= screen.cols) cursorCol = screen.cols - 1
    }

    private fun carriageReturn() {
        cursorCol = 0
    }

    private fun lineFeed() {
        if (cursorRow >= screen.scrollBottom) {
            screen.scrollUp()
        } else if (cursorRow < screen.rows - 1) {
            cursorRow++
        }
    }

    private enum class State {
        GROUND,
        ESCAPE,
        ESC_IGNORE_ONE,
        CSI_PARAM,
        OSC,
        OSC_ESC,
        DCS,
        DCS_ESC,
    }
}
