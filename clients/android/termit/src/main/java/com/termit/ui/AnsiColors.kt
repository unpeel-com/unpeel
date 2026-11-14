package com.termit.ui

import kotlin.math.pow

/**
 * Palettes matching Unpeel Mac's Ghostty theme ([Theme.swift] TerminalPaneStyle),
 * not Ghostty's stock Name.default. The phone picks light or dark from its own
 * system appearance. Cube/grey stay xterm-identical to Ghostty Palette.default.
 *
 * Cell color ints are either an index 0..258, or an ARGB truecolor (alpha == 0xFF).
 * A default foreground is resolved later against the cell's own background.
 */
object AnsiColors {
    const val INDEX_FG = 256
    const val INDEX_BG = 257
    const val INDEX_CURSOR = 258
    const val NUM_INDEXED = 259

    /** Mac light terminal ink `#09090b` and paper `#ffffff`. */
    const val LIGHT_FOREGROUND = 0xff09090b.toInt()
    const val LIGHT_BACKGROUND = 0xffffffff.toInt()

    /** Mac dark terminal ink `#fafafa` and canvas `#1A1B1D`. */
    const val DARK_FOREGROUND = 0xfffafafa.toInt()
    const val DARK_BACKGROUND = 0xff1a1b1d.toInt()

    /**
     * Point where dark ink contrasts more than light ink
     * (`sqrt(1.05 * 0.05) - 0.05` from the WCAG contrast ratio).
     */
    private const val LIGHT_BACKGROUND_LUMINANCE = 0.179

    private val DARK_ANSI = intArrayOf(
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

    /** Mac [TerminalPaneStyle.light] ANSI 0–15. */
    private val LIGHT_ANSI = intArrayOf(
        0xff09090b.toInt(),
        0xffdc2626.toInt(),
        0xff16a34a.toInt(),
        0xffca8a04.toInt(),
        0xff2563eb.toInt(),
        0xff9333ea.toInt(),
        0xff0891b2.toInt(),
        0xffe4e4e7.toInt(),
        0xff71717a.toInt(),
        0xffef4444.toInt(),
        0xff22c55e.toInt(),
        0xffeab308.toInt(),
        0xff3b82f6.toInt(),
        0xffa855f7.toInt(),
        0xff06b6d4.toInt(),
        0xfffafafa.toInt(),
    )

    /** Dark palette. Existing callers that have not learned the phone appearance yet. */
    fun buildDefaultPalette(): IntArray = buildPalette(dark = true)

    /** [dark] false is the Mac light terminal; true is the Mac dark terminal. */
    fun buildPalette(dark: Boolean): IntArray {
        val p = IntArray(NUM_INDEXED)
        fillCubeAndGrey(p)
        installTheme(p, dark)
        return p
    }

    /**
     * Write ANSI 0–15 plus default fg/bg/cursor for [dark].
     * Leaves the 16–255 cube and greys alone, so a theme change does not
     * rebuild xterm colors a program already indexed.
     */
    fun installTheme(palette: IntArray, dark: Boolean) {
        val named = if (dark) DARK_ANSI else LIGHT_ANSI
        named.copyInto(palette)
        val foreground = if (dark) DARK_FOREGROUND else LIGHT_FOREGROUND
        val background = if (dark) DARK_BACKGROUND else LIGHT_BACKGROUND
        palette[INDEX_FG] = foreground
        palette[INDEX_BG] = background
        palette[INDEX_CURSOR] = foreground
    }

    /**
     * Ink for a glyph whose SGR foreground is still the default.
     * A light cell (a CLI painted for a light terminal) gets `#09090b`;
     * a dark cell gets `#fafafa`.
     */
    fun contrastingDefaultForeground(background: Int): Int =
        if (isLightBackground(background)) LIGHT_FOREGROUND else DARK_FOREGROUND

    fun isLightBackground(argb: Int): Boolean {
        val r = srgbChannel((argb ushr 16) and 0xff)
        val g = srgbChannel((argb ushr 8) and 0xff)
        val b = srgbChannel(argb and 0xff)
        val luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b
        return luminance > LIGHT_BACKGROUND_LUMINANCE
    }

    private fun srgbChannel(channel: Int): Double {
        val c = channel / 255.0
        return if (c <= 0.04045) c / 12.92 else ((c + 0.055) / 1.055).pow(2.4)
    }

    private fun fillCubeAndGrey(p: IntArray) {
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
