package com.termit.ui

/**
 * Palette matching Unpeel Mac's Ghostty theme ([Theme.swift] TerminalPaneStyle.dark),
 * not Ghostty's stock Name.default (which is muted Tomorrow-Night and looks washed
 * next to the Mac pane). Cube/grey stay xterm-identical to Ghostty Palette.default.
 *
 * Cell color ints are either an index 0..258, or an ARGB truecolor (alpha == 0xFF).
 */
object AnsiColors {
    const val INDEX_FG = 256
    const val INDEX_BG = 257
    const val INDEX_CURSOR = 258
    const val NUM_INDEXED = 259

    /**
     * Unpeel dark ANSI 0–15 (same strings GhosttyBridge feeds into libghostty):
     * ```
     * 0  #1c1c22  1 #ef4444  2 #22c55e  3 #eab308
     * 4  #3b82f6  5 #a855f7  6 #06b6d4  7 #a1a1aa
     * 8  #6e6e76  9 #f87171 10 #4ade80 11 #facc15
     * 12 #60a5fa 13 #c084fc 14 #22d3ee 15 #fafafa
     * ```
     * fg/cursor #fafafa, bg #1A1B1D (HostTerminal.CANVAS may re-apply BG).
     */
    fun buildDefaultPalette(): IntArray {
        val p = IntArray(NUM_INDEXED)
        val named = intArrayOf(
            0xff1c1c22.toInt(), // black
            0xffef4444.toInt(), // red
            0xff22c55e.toInt(), // green
            0xffeab308.toInt(), // yellow
            0xff3b82f6.toInt(), // blue
            0xffa855f7.toInt(), // magenta
            0xff06b6d4.toInt(), // cyan
            0xffa1a1aa.toInt(), // white
            0xff6e6e76.toInt(), // bright black
            0xfff87171.toInt(), // bright red
            0xff4ade80.toInt(), // bright green
            0xfffacc15.toInt(), // bright yellow
            0xff60a5fa.toInt(), // bright blue
            0xffc084fc.toInt(), // bright magenta
            0xff22d3ee.toInt(), // bright cyan
            0xfffafafa.toInt(), // bright white
        )
        named.copyInto(p)

        // 216-color cube: Ghostty `if (c == 0) 0 else c * 40 + 55`
        var i = 16
        for (r in 0..5) for (g in 0..5) for (b in 0..5) {
            fun level(v: Int) = if (v == 0) 0 else v * 40 + 55
            p[i++] = 0xff000000.toInt() or (level(r) shl 16) or (level(g) shl 8) or level(b)
        }
        // 24 greys: ((i - 232) * 10) + 8
        for (g in 0 until 24) {
            val c = g * 10 + 8
            p[i++] = 0xff000000.toInt() or (c shl 16) or (c shl 8) or c
        }

        p[INDEX_FG] = 0xfffafafa.toInt()
        p[INDEX_BG] = 0xff1a1b1d.toInt()
        p[INDEX_CURSOR] = 0xfffafafa.toInt()
        return p
    }

    /** True when [color] carries a packed 24-bit ARGB (same rule as Termux TextStyle). */
    fun isTruecolor(color: Int): Boolean = (color.toLong() and 0xff000000L) == 0xff000000L

    fun resolve(color: Int, palette: IntArray, bold: Boolean = false, dim: Boolean = false): Int {
        var argb = if (isTruecolor(color)) {
            color
        } else {
            // Bold is glyph weight only — Mac Ghostty also defaults bold-color unset
            // (no 0–7 → 8–15 remap).
            @Suppress("UNUSED_VARIABLE")
            val ignored = bold
            val idx = color.coerceIn(0, NUM_INDEXED - 1)
            palette[idx]
        }
        if (dim) {
            val r = ((argb shr 16) and 0xff) * 2 / 3
            val g = ((argb shr 8) and 0xff) * 2 / 3
            val b = (argb and 0xff) * 2 / 3
            argb = 0xff000000.toInt() or (r shl 16) or (g shl 8) or b
        }
        return argb
    }
}
