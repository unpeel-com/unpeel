package com.termit.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AnsiColorsTest {
    @Test
    fun darkPaletteMatchesMacDarkTerminal() {
        val palette = AnsiColors.buildPalette(dark = true)
        assertEquals(0xff1c1c22.toInt(), palette[0])
        assertEquals(0xfffafafa.toInt(), palette[15])
        assertEquals(AnsiColors.DARK_FOREGROUND, palette[AnsiColors.INDEX_FG])
        assertEquals(AnsiColors.DARK_BACKGROUND, palette[AnsiColors.INDEX_BG])
        assertEquals(AnsiColors.DARK_FOREGROUND, palette[AnsiColors.INDEX_CURSOR])
    }

    @Test
    fun lightPaletteMatchesMacLightTerminal() {
        val palette = AnsiColors.buildPalette(dark = false)
        assertEquals(0xff09090b.toInt(), palette[0])
        assertEquals(0xffe4e4e7.toInt(), palette[7])
        assertEquals(0xfffafafa.toInt(), palette[15])
        assertEquals(AnsiColors.LIGHT_FOREGROUND, palette[AnsiColors.INDEX_FG])
        assertEquals(AnsiColors.LIGHT_BACKGROUND, palette[AnsiColors.INDEX_BG])
        assertEquals(AnsiColors.LIGHT_FOREGROUND, palette[AnsiColors.INDEX_CURSOR])
    }

    @Test
    fun installThemeLeavesTheXtermCube() {
        val palette = AnsiColors.buildPalette(dark = true)
        val cube = palette[16]
        AnsiColors.installTheme(palette, dark = false)
        assertEquals(cube, palette[16])
        assertEquals(AnsiColors.LIGHT_FOREGROUND, palette[0])
        assertEquals(AnsiColors.LIGHT_BACKGROUND, palette[AnsiColors.INDEX_BG])
    }

    @Test
    fun defaultForegroundContrastsWithTheCellBackground() {
        assertEquals(AnsiColors.LIGHT_FOREGROUND, AnsiColors.contrastingDefaultForeground(0xfff4f4f5.toInt()))
        assertEquals(AnsiColors.LIGHT_FOREGROUND, AnsiColors.contrastingDefaultForeground(AnsiColors.LIGHT_BACKGROUND))
        assertEquals(AnsiColors.DARK_FOREGROUND, AnsiColors.contrastingDefaultForeground(AnsiColors.DARK_BACKGROUND))
        assertEquals(AnsiColors.DARK_FOREGROUND, AnsiColors.contrastingDefaultForeground(0xff141414.toInt()))
        assertTrue(AnsiColors.isLightBackground(0xffd4d4d8.toInt()))
        assertFalse(AnsiColors.isLightBackground(0xff3a3a40.toInt()))
    }
}
