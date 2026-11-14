package com.termit.ui

import android.content.Context

/** Font metrics + renderer for Unpeel's HostTerminalView (Termux TerminalMetrics stand-in). */
class TerminalMetrics(context: Context, textSizePx: Int) {
    val renderer = TerminalRenderer(textSizePx, TermitFonts.regular(context))
    val fontWidth: Float = renderer.fontWidth
    val fontLineSpacing: Int = renderer.fontLineSpacing
    val fontLineSpacingAndAscent: Int = renderer.fontLineSpacingAndAscent
}
