package com.unpeel.android

import androidx.compose.ui.graphics.toArgb
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MacChromeTest {
    @Test
    fun lightAndDarkPairsMatchThemeSwift() {
        assertEquals(0xFFFFFFFF.toInt(), MacChrome.appBackground(false))
        assertEquals(0xFF121314.toInt(), MacChrome.appBackground(true))
        assertEquals(0xFFFFFFFF.toInt(), MacChrome.surface(false))
        assertEquals(0xFF1A1B1D.toInt(), MacChrome.surface(true))
        assertEquals(0xFF111217.toInt(), MacChrome.foreground(false))
        assertEquals(0xFFF3F5FB.toInt(), MacChrome.foreground(true))
        assertEquals(0x99111217.toInt(), MacChrome.muted(false))
        assertEquals(0xA8F3F5FB.toInt(), MacChrome.muted(true))
        assertEquals(0x1A111217, MacChrome.hover(false))
        assertEquals(0x1AF3F5FB, MacChrome.hover(true))
        assertEquals(0xFFFFFFFF.toInt(), MacChrome.activeRow(false))
        assertEquals(0x29FFFFFF, MacChrome.activeRow(true))
        assertEquals(0x14000000, MacChrome.resizer(false))
        assertEquals(0x0EFFFFFF, MacChrome.resizer(true))
        assertEquals(0x1A000000, MacChrome.paneDivider(false))
        assertEquals(0x24FFFFFF, MacChrome.paneDivider(true))
        assertEquals(0xFF4A4D55.toInt(), MacChrome.spinner(false))
        assertEquals(0xFFB9BDC9.toInt(), MacChrome.spinner(true))
        assertEquals(0xFF34C759.toInt(), Accent.toArgb())
        assertEquals(0xFFEF4444.toInt(), Danger.toArgb())
        assertEquals(0xFFF59E0B.toInt(), Attention.toArgb())
        assertEquals(0xFF60A5FA.toInt(), Unread.toArgb())
    }

    @Test
    fun sheetAndPanelUseMacSurfaceOnlyInLight() {
        assertEquals(MacChrome.surface(false), MacChrome.sheet(false))
        assertEquals(MacChrome.surface(false), MacChrome.panel(false))
        assertEquals(0xFF1B1C22.toInt(), MacChrome.sheet(true))
        assertEquals(0xFF16171C.toInt(), MacChrome.panel(true))
    }

    @Test
    fun folderSwatchesUseTheMacPairs() {
        assertEquals(0xFF2095C9.toInt(), MacChrome.folder("sky", false))
        assertEquals(0xFF7DD3FC.toInt(), MacChrome.folder("sky", true))
        assertEquals(0xFF4F73E6.toInt(), MacChrome.folder("blue", false))
        assertEquals(0xFF7EA6FF.toInt(), MacChrome.folder("blue", true))
        assertEquals(0xFF7B5BDA.toInt(), MacChrome.folder("violet", false))
        assertEquals(0xFFB79CFF.toInt(), MacChrome.folder("violet", true))
        assertEquals(0xFFD75F8F.toInt(), MacChrome.folder("rose", false))
        assertEquals(0xFFF79AC0.toInt(), MacChrome.folder("rose", true))
        assertEquals(0xFFB87511.toInt(), MacChrome.folder("amber", false))
        assertEquals(0xFFF8C86A.toInt(), MacChrome.folder("amber", true))
        assertEquals(0xFF5F9A3D.toInt(), MacChrome.folder("moss", false))
        assertEquals(0xFF9DD67A.toInt(), MacChrome.folder("moss", true))
        assertEquals(0xFF159B91.toInt(), MacChrome.folder("teal", false))
        assertEquals(0xFF64DCCB.toInt(), MacChrome.folder("teal", true))
        assertEquals(0xFF687083.toInt(), MacChrome.folder("graphite", false))
        assertEquals(0xFFB8BCC8.toInt(), MacChrome.folder("graphite", true))
        assertNull(MacChrome.folder("nope", false))
        assertNull(MacChrome.folder(null, true))
    }
}
