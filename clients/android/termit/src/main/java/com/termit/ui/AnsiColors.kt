package com.termit.ui

import android.graphics.Color

object AnsiColors {
    private val palette = intArrayOf(
        Color.rgb(0, 0, 0),
        Color.rgb(205, 49, 49),
        Color.rgb(13, 188, 121),
        Color.rgb(229, 229, 16),
        Color.rgb(36, 114, 200),
        Color.rgb(188, 63, 188),
        Color.rgb(17, 168, 205),
        Color.rgb(229, 229, 229),
        Color.rgb(102, 102, 102),
        Color.rgb(241, 76, 76),
        Color.rgb(35, 209, 139),
        Color.rgb(245, 245, 67),
        Color.rgb(59, 142, 234),
        Color.rgb(214, 112, 214),
        Color.rgb(41, 184, 219),
        Color.rgb(255, 255, 255),
    )

    fun foreground(index: Int, bold: Boolean): Int {
        val i = index.coerceIn(0, 15)
        return if (bold && i < 8) palette[i + 8] else palette[i]
    }

    fun background(index: Int): Int = palette[index.coerceIn(0, 15)]
}
