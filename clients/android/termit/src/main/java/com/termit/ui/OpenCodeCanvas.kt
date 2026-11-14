package com.termit.ui

/**
 * OpenCode TUI canvas pairs.
 *
 * Source: Mac `OpenCodeThemeResolver.builtInBackgrounds` in
 * `clients/native/UnpeelNative/Sources/UnpeelNative/OpenCodeTheme.swift`.
 * That table is `theme.background`, the frame behind the TUI, not a
 * per-cell truecolor palette. Cell colors stay as painted.
 *
 * A hex maps only when every built-in theme that uses it on that side
 * agrees on the other side. Themes whose light and dark canvases are the
 * same color are omitted. `#FFFFFF`, `#FAFAFA`, and `#FDF6E3` are the
 * light end of more than one dark canvas, so a dark phone leaves them.
 */
object OpenCodeCanvas {
    private val themes = listOf(
        0xFFFFFF to 0x0A0A0A, // opencode, oc-1, orng
        0x0F0F0F to 0x0F0F0F, // aura
        0x0B0E14 to 0x0B0E14, // ayu
        0xFFFFFF to 0x161616, // carbonfox
        0xEFF1F5 to 0x1E1E2E, // catppuccin, catppuccin-mocha
        0x303446 to 0x303446, // catppuccin-frappe
        0x24273A to 0x24273A, // catppuccin-macchiato
        0xFFFFFF to 0x193549, // cobalt2
        0xFCFCFC to 0x181818, // cursor
        0xF8F8F2 to 0x282A36, // dracula
        0xFDF6E3 to 0x2D353B, // everforest
        0xFFFCF0 to 0x100F0F, // flexoki
        0xFFFFFF to 0x0D1117, // github
        0xFBF1C7 to 0x282828, // gruvbox
        0xF2E9DE to 0x1F1F28, // kanagawa
        0xFAFAFA to 0x263238, // material
        0xEEF3EA to 0x0A0E0A, // matrix
        0xFFFFFF to 0x171721, // mercury
        0xFAFAFA to 0x272822, // monokai
        0x011627 to 0x011627, // night-owl, nightowl
        0xECEFF4 to 0x2E3440, // nord
        0xFAFAFA to 0x282C34, // one-dark, one-dark-pro, onedarkpro
        0xF6F5DD to 0x111C18, // osaka-jade
        0xFAFAFA to 0x292D3E, // palenight
        0xFAF4ED to 0x191724, // rosepine, rose-pine
        0xF7EBFF to 0x1A102B, // shadesofpurple, shades-of-purple
        0xFDF6E3 to 0x002B36, // solarized
        0xFAFAFA to 0x262335, // synthwave84
        0xE1E2E7 to 0x1A1B26, // tokyonight, tokyo-night
        0xFFFFFF to 0x000000, // vercel
        0xFFFFFF to 0x101010, // vesper
        0xFFFFEF to 0x3F3F3F, // zenburn
    )

    private val darkToLight: Map<Int, Int>
    private val lightToDark: Map<Int, Int>

    init {
        val paired = themes.filter { (light, dark) -> light != dark }
        darkToLight = uniqueOtherSide(paired) { it.second to it.first }
        lightToDark = uniqueOtherSide(paired) { it.first to it.second }
    }

    /** [argb] is `0xAARRGGBB`. A miss returns [argb]. */
    fun map(argb: Int, night: Boolean): Int {
        val rgb = argb and 0xFFFFFF
        val mapped = (if (night) lightToDark[rgb] else darkToLight[rgb]) ?: return argb
        return (argb and 0xFF000000.toInt()) or mapped
    }

    private fun uniqueOtherSide(
        pairs: List<Pair<Int, Int>>,
        side: (Pair<Int, Int>) -> Pair<Int, Int>,
    ): Map<Int, Int> {
        val grouped = HashMap<Int, MutableSet<Int>>()
        for (pair in pairs) {
            val (key, value) = side(pair)
            grouped.getOrPut(key) { HashSet() }.add(value)
        }
        val unique = HashMap<Int, Int>(grouped.size)
        for ((key, values) in grouped) {
            if (values.size == 1) unique[key] = values.first()
        }
        return unique
    }
}
