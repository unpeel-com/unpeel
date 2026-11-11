package com.termux.view;

import android.graphics.Typeface;

/** Unpeel: reads package-visible font metrics without editing {@link TerminalRenderer}. */
public final class TerminalMetrics {
    public final TerminalRenderer renderer;
    public final float fontWidth;
    public final int fontLineSpacing;
    public final int fontLineSpacingAndAscent;

    public TerminalMetrics(int textSizePx) {
        renderer = new TerminalRenderer(textSizePx, Typeface.MONOSPACE);
        fontWidth = renderer.mFontWidth;
        fontLineSpacing = renderer.mFontLineSpacing;
        fontLineSpacingAndAscent = renderer.mFontLineSpacingAndAscent;
    }
}
