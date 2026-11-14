package com.termit.ui

/**
 * GrokNight ↔ GrokDay truecolor pairs.
 *
 * Source: xai-org/grok-build
 * `crates/codegen/xai-grok-pager-render/src/theme/groknight.rs`
 * `crates/codegen/xai-grok-pager-render/src/theme/grokday.rs`
 *
 * Same `Theme` field on each side. A truecolor cell that matches one side
 * is drawn as the other side when the phone appearance disagrees.
 * Colors that are not in this table stay as painted.
 *
 * Night magenta `#BB9AF7` is both `MAGENTA` and `accent_verify`. The day
 * side of that shared value is magenta `#7D4BC6`. Day-only verify
 * `#7850A0` maps back to `#BB9AF7`. Day blue `#2F64D2` is both `BLUE` and
 * `link_fg`; the night side of that shared value is blue `#7AA2F7`.
 * Night link `#7AA6DA` still maps to `#2F64D2`.
 */
object GrokTruecolor {
    private val nightToDay = HashMap<Int, Int>(4096)
    private val dayToNight = HashMap<Int, Int>(4096)

    init {
        // Palette constants, then Theme fields that are not those constants.
        // The first writer wins, so a shared day color keeps the palette role.
        pair(0x0A0A0A, 0xF5F5F5) // bg_terminal
        pair(0x0C0C0C, 0xF0F0F0) // bg_dark
        pair(0x111111, 0xEAEAEA) // bg_storm_dark, scrollbar
        pair(0x141414, 0xEEEEEE) // bg_base
        pair(0x242424, 0xDEDEDE) // bg_highlight
        pair(0xE1E1E1, 0x262626) // text_primary
        pair(0xC8C8C8, 0x444444) // text_secondary
        pair(0x414141, 0xB2B2B2) // gutter, paste_dim
        pair(0x6C6C6C, 0x767676) // gray / comment
        pair(0x5A5A5A, 0x8E8E8E) // dark3
        pair(0x787878, 0x626262) // gray_bright
        pair(0x7AA2F7, 0x2F64D2) // blue; day link uses this blue too
        pair(0x3D59A1, 0x28448A) // blue0
        pair(0x3A95AB, 0x0F87A2) // blue1
        pair(0x7DCFFF, 0x0082AA) // cyan
        pair(0x9ECE6A, 0x378E23) // green
        pair(0x73DACA, 0x0C947C) // green1
        pair(0xBB9AF7, 0x7D4BC6) // magenta
        pair(0xFF9E64, 0xC3691E) // orange
        pair(0x9D7CD8, 0x6C3EB2) // purple
        pair(0xF7768E, 0xCD3048) // red
        pair(0xDB4B4B, 0xAF2323) // red1
        pair(0x1ABC9C, 0x0A8E70) // teal
        pair(0xE0AF68, 0xA27612) // yellow
        pair(0x420E14, 0xF5DADE) // diff delete background
        pair(0x063806, 0xDAF2DC) // diff insert background
        pair(0x1C1C1C, 0xE4E4E4) // code block background
        pair(0x2C2C2C, 0xD0D0D0) // bg_hover
        pair(0x585858, 0xA5A5A5) // gray_dim
        pair(0xFFDB8D, 0xA8780A) // accent_plan
        pair(0x8BC34A, 0x4CAF50) // accent_remember
        pair(0x3C3C41, 0xB9B9BE) // selection_border
        pair(0x323237, 0xC8C8CD) // prompt_border
        pair(0x505058, 0xA5A5AF) // prompt_border_active
        pair(0x1E1E22, 0xD4D4D8) // hover_border
        pair(0x363636, 0xC6C6C6) // bg_visual
        pair(0x7AA6DA, 0x2F64D2) // link_fg; day slot already owned by blue
        val themePairs = nightToDay.entries.map { it.key to it.value }
        // Pushed sticky header. scrollback_pane.rs fades the band toward
        // bg_base at opacity = visible_rows / (render_height + 1).
        // At 1/3, bg_light #242424 becomes #191919. GrokDay is #E9E9E9.
        pair(0x191919, 0xE9E9E9)
        // At 5/6 the same blend is the user band that stays dark on a
        // light phone: bg #212121, text #BFBFBF, chevron #AAAAAA, time
        // #5D5D5D. GrokDay at 5/6 is #E1E1E1, #474747, #606060, #8A8A8A.
        // #E1E1E1 is already night text_primary, so it has no reverse entry.
        pair(0xBFBFBF, 0x474747)
        pair(0xAAAAAA, 0x606060)
        pair(0x5D5D5D, 0x8A8A8A)
        nightToDay.putIfAbsent(0x212121, 0xE1E1E1)
        // At 2/3: bg #1F1F1F, text #9D9D9D, chevron #8C8C8C, time #4F4F4F.
        // GrokDay is #E3E3E3, #696969, #7D7D7D, #9E9E9E.
        pair(0x1F1F1F, 0xE3E3E3)
        pair(0x9D9D9D, 0x696969)
        pair(0x8C8C8C, 0x7D7D7D)
        pair(0x4F4F4F, 0x9E9E9E)
        // At 2/5: bg #1A1A1A, text #666666, chevron #5C5C5C.
        // GrokDay is #E8E8E8, #9E9E9E, #AAAAAA. #AAAAAA is already a night
        // chevron, so it has no reverse entry.
        pair(0x1A1A1A, 0xE8E8E8)
        pair(0x666666, 0x9E9E9E)
        nightToDay.putIfAbsent(0x5C5C5C, 0xAAAAAA)
        // Blank row of the same fade. bg_light toward bg_base lands on
        // #171717 for the ratios around 1/5; GrokDay is #EBEBEB.
        pair(0x171717, 0xEBEBEB)
        // Day accent_verify has no distinct night twin.
        dayToNight.putIfAbsent(0x7850A0, 0xBB9AF7)
        // Every other mix of those theme colors. render_height is clamped to
        // the viewport; 255 covers a phone grid. A night shade several mixes
        // land on keeps the theme color closest to that shade.
        addStickyHeaderFades(themePairs)
    }

    /**
     * `opacity = visible_rows / (render_height + 1)`, then each channel is
     * `round(bg_base * (1 - opacity) + original * opacity)`.
     */
    private fun addStickyHeaderFades(theme: List<Pair<Int, Int>>) {
        val nightBase = 0x141414
        val dayBase = 0xEEEEEE
        val nightKeys = theme.mapTo(HashSet()) { it.first }
        val best = HashMap<Int, FadeChoice>(4096)
        for (rows in 1..255) {
            val denom = (rows + 1).toFloat()
            for (visible in 1..rows) {
                val opacity = visible.toFloat() / denom
                for ((night, day) in theme) {
                    val fadedNight = blend(nightBase, night, opacity)
                    val fadedDay = blend(dayBase, day, opacity)
                    val distance = rgbDistance(night, fadedNight)
                    val prev = best[fadedNight]
                    if (
                        prev == null ||
                        distance < prev.distance ||
                        (distance == prev.distance && night < prev.source)
                    ) {
                        best[fadedNight] = FadeChoice(distance, night, fadedDay)
                    }
                }
            }
        }
        for ((fadedNight, chosen) in best) {
            if (nightToDay.putIfAbsent(fadedNight, chosen.day) == null && chosen.day !in nightKeys) {
                dayToNight.putIfAbsent(chosen.day, fadedNight)
            }
        }
    }

    private fun blend(base: Int, original: Int, opacity: Float): Int {
        fun channel(shift: Int): Int {
            val from = (base shr shift) and 0xFF
            val to = (original shr shift) and 0xFF
            val mixed = from * (1f - opacity) + to * opacity
            return kotlin.math.round(mixed).toInt().coerceIn(0, 255) shl shift
        }
        return channel(16) or channel(8) or channel(0)
    }

    private fun rgbDistance(a: Int, b: Int): Int {
        fun channel(shift: Int): Int = kotlin.math.abs(((a shr shift) and 0xFF) - ((b shr shift) and 0xFF))
        return channel(16) + channel(8) + channel(0)
    }

    private class FadeChoice(val distance: Int, val source: Int, val day: Int)

    /**
     * [night] is the phone appearance. A light phone rewrites GrokNight
     * colors to GrokDay. A dark phone rewrites GrokDay colors to GrokNight.
     */
    fun map(argb: Int, night: Boolean): Int {
        val rgb = argb and 0xFFFFFF
        val mapped = if (night) dayToNight[rgb] else nightToDay[rgb]
        return if (mapped == null) argb else 0xFF000000.toInt() or mapped
    }

    private fun pair(night: Int, day: Int) {
        nightToDay.putIfAbsent(night, day)
        dayToNight.putIfAbsent(day, night)
    }
}
