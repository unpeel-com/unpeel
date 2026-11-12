package com.termit.ui

import android.graphics.Typeface

/** Font metrics + renderer for Unpeel's HostTerminalView (Termux TerminalMetrics stand-in). */
class TerminalMetrics(textSizePx: Int) {
    val renderer = TerminalRenderer(textSizePx, Typeface.MONOSPACE)
    val fontWidth: Float = renderer.fontWidth
    val fontLineSpacing: Int = renderer.fontLineSpacing
    val fontLineSpacingAndAscent: Int = renderer.fontLineSpacingAndAscent
}
