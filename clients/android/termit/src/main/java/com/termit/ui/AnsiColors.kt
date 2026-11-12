package com.termit.ui

/**
 * XTerm 256-color chart (Termux TerminalColorScheme), plus default fg/bg/cursor slots.
 * Cell color ints are either an index 0..258, or an ARGB truecolor (alpha == 0xFF).
 */
object AnsiColors {
    const val INDEX_FG = 256
    const val INDEX_BG = 257
    const val INDEX_CURSOR = 258
    const val NUM_INDEXED = 259

    /** True when [color] carries a packed 24-bit ARGB (same rule as Termux TextStyle). */
    fun isTruecolor(color: Int): Boolean = (color.toLong() and 0xff000000L) == 0xff000000L

    fun buildDefaultPalette(): IntArray {
        val p = IntArray(NUM_INDEXED)
        // 16 system colors (dim then bright) — Termux defaults.
        val basic = intArrayOf(
            0xff000000.toInt(), 0xffcd0000.toInt(), 0xff00cd00.toInt(), 0xffcdcd00.toInt(),
            0xff6495ed.toInt(), 0xffcd00cd.toInt(), 0xff00cdcd.toInt(), 0xffe5e5e5.toInt(),
            0xff7f7f7f.toInt(), 0xffff0000.toInt(), 0xff00ff00.toInt(), 0xffffff00.toInt(),
            0xff5c5cff.toInt(), 0xffff00ff.toInt(), 0xff00ffff.toInt(), 0xffffffff.toInt(),
        )
        basic.copyInto(p)
        // 216-color cube.
        var i = 16
        for (r in 0..5) for (g in 0..5) for (b in 0..5) {
            fun level(v: Int) = if (v == 0) 0 else 95 + 40 * (v - 1)
            p[i++] = 0xff000000.toInt() or (level(r) shl 16) or (level(g) shl 8) or level(b)
        }
        // 24 greys.
        for (g in 0 until 24) {
            val c = 8 + g * 10
            p[i++] = 0xff000000.toInt() or (c shl 16) or (c shl 8) or c
        }
        p[INDEX_FG] = 0xffffffff.toInt()
        p[INDEX_BG] = 0xff1a1b1d.toInt() // Unpeel canvas default
        p[INDEX_CURSOR] = 0xffffffff.toInt()
        return p
    }

    fun resolve(color: Int, palette: IntArray, bold: Boolean = false, dim: Boolean = false): Int {
        val argb = if (isTruecolor(color)) {
            color
        } else {
            val idx = color.coerceIn(0, NUM_INDEXED - 1)
            val bright = if (bold && idx in 0..7) idx + 8 else idx
            palette[bright]
        }
        if (!dim) return argb
        // xterm / libvte dim: each channel * 2/3
        val r = ((argb shr 16) and 0xff) * 2 / 3
        val g = ((argb shr 8) and 0xff) * 2 / 3
        val b = (argb and 0xff) * 2 / 3
        return 0xff000000.toInt() or (r shl 16) or (g shl 8) or b
    }
}
