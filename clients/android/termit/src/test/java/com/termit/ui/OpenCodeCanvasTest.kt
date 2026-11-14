package com.termit.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class OpenCodeCanvasTest {
    @Test
    fun lightPhoneRewritesAUniqueDarkCanvas() {
        assertEquals(0xFFFFFFFF.toInt(), OpenCodeCanvas.map(0xFF0A0A0A.toInt(), night = false))
        assertEquals(0xFFEFF1F5.toInt(), OpenCodeCanvas.map(0xFF1E1E2E.toInt(), night = false))
        assertEquals(0xFFE1E2E7.toInt(), OpenCodeCanvas.map(0xFF1A1B26.toInt(), night = false))
        assertEquals(0xFFF8F8F2.toInt(), OpenCodeCanvas.map(0xFF282A36.toInt(), night = false))
        assertEquals(0xFFFFFFFF.toInt(), OpenCodeCanvas.map(0xFF000000.toInt(), night = false))
    }

    @Test
    fun darkPhoneRewritesOnlyAUniqueLightCanvas() {
        assertEquals(0xFF1E1E2E.toInt(), OpenCodeCanvas.map(0xFFEFF1F5.toInt(), night = true))
        assertEquals(0xFF1A1B26.toInt(), OpenCodeCanvas.map(0xFFE1E2E7.toInt(), night = true))
        assertEquals(0xFF282A36.toInt(), OpenCodeCanvas.map(0xFFF8F8F2.toInt(), night = true))
        assertEquals(0xFFFFFFFF.toInt(), OpenCodeCanvas.map(0xFFFFFFFF.toInt(), night = true))
        assertEquals(0xFFFAFAFA.toInt(), OpenCodeCanvas.map(0xFFFAFAFA.toInt(), night = true))
        assertEquals(0xFFFDF6E3.toInt(), OpenCodeCanvas.map(0xFFFDF6E3.toInt(), night = true))
    }

    @Test
    fun sameColorThemesAndUnknownColorsStay() {
        assertEquals(0xFF0F0F0F.toInt(), OpenCodeCanvas.map(0xFF0F0F0F.toInt(), night = false))
        assertEquals(0xFF011627.toInt(), OpenCodeCanvas.map(0xFF011627.toInt(), night = true))
        assertEquals(0xFF141414.toInt(), OpenCodeCanvas.map(0xFF141414.toInt(), night = false))
        assertEquals(0xFF123456.toInt(), OpenCodeCanvas.map(0xFF123456.toInt(), night = true))
    }
}
