package com.termit.ui

import android.content.Context
import android.graphics.Typeface

/** JetBrains Mono — same face Mac Unpeel / Ghostty ships. Falls back to device monospace. */
object TermitFonts {
    @Volatile private var regular: Typeface? = null

    fun regular(context: Context): Typeface {
        regular?.let { return it }
        return synchronized(this) {
            regular ?: run {
                val loaded = runCatching {
                    Typeface.createFromAsset(context.assets, "fonts/JetBrainsMono-Regular.ttf")
                }.getOrElse { Typeface.MONOSPACE }
                regular = loaded
                loaded
            }
        }
    }
}
