package com.termit.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class GrokTruecolorTest {
    @Test
    fun lightPhoneRewritesGrokNight() {
        assertEquals(0xFFEEEEEE.toInt(), GrokTruecolor.map(0xFF141414.toInt(), night = false))
        assertEquals(0xFF262626.toInt(), GrokTruecolor.map(0xFFE1E1E1.toInt(), night = false))
        assertEquals(0xFF2F64D2.toInt(), GrokTruecolor.map(0xFF7AA2F7.toInt(), night = false))
        assertEquals(0xFF2F64D2.toInt(), GrokTruecolor.map(0xFF7AA6DA.toInt(), night = false))
        assertEquals(0xFF7D4BC6.toInt(), GrokTruecolor.map(0xFFBB9AF7.toInt(), night = false))
        assertEquals(0xFFE9E9E9.toInt(), GrokTruecolor.map(0xFF191919.toInt(), night = false))
        assertEquals(0xFFE1E1E1.toInt(), GrokTruecolor.map(0xFF212121.toInt(), night = false))
        assertEquals(0xFF474747.toInt(), GrokTruecolor.map(0xFFBFBFBF.toInt(), night = false))
        assertEquals(0xFF606060.toInt(), GrokTruecolor.map(0xFFAAAAAA.toInt(), night = false))
        assertEquals(0xFF8A8A8A.toInt(), GrokTruecolor.map(0xFF5D5D5D.toInt(), night = false))
        assertEquals(0xFF262626.toInt(), GrokTruecolor.map(0xFFE1E1E1.toInt(), night = false))
        assertEquals(0xFFE3E3E3.toInt(), GrokTruecolor.map(0xFF1F1F1F.toInt(), night = false))
        assertEquals(0xFF696969.toInt(), GrokTruecolor.map(0xFF9D9D9D.toInt(), night = false))
        assertEquals(0xFFE8E8E8.toInt(), GrokTruecolor.map(0xFF1A1A1A.toInt(), night = false))
        assertEquals(0xFF9E9E9E.toInt(), GrokTruecolor.map(0xFF666666.toInt(), night = false))
        assertEquals(0xFFEBEBEB.toInt(), GrokTruecolor.map(0xFF171717.toInt(), night = false))
    }

    @Test
    fun lightPhoneRewritesEveryStickyBackgroundStep() {
        for (level in 0x0D..0x23) {
            val night = 0xFF000000.toInt() or (level * 0x010101)
            val mapped = GrokTruecolor.map(night, night = false)
            val red = (mapped shr 16) and 0xFF
            org.junit.Assert.assertTrue(
                "gray %02X stayed %06X".format(level, mapped and 0xFFFFFF),
                red >= 0xD0,
            )
        }
    }

    @Test
    fun darkPhoneRewritesGrokDay() {
        assertEquals(0xFF141414.toInt(), GrokTruecolor.map(0xFFEEEEEE.toInt(), night = true))
        assertEquals(0xFFE1E1E1.toInt(), GrokTruecolor.map(0xFF262626.toInt(), night = true))
        assertEquals(0xFF7AA2F7.toInt(), GrokTruecolor.map(0xFF2F64D2.toInt(), night = true))
        assertEquals(0xFFBB9AF7.toInt(), GrokTruecolor.map(0xFF7850A0.toInt(), night = true))
        assertEquals(0xFF191919.toInt(), GrokTruecolor.map(0xFFE9E9E9.toInt(), night = true))
        assertEquals(0xFFBFBFBF.toInt(), GrokTruecolor.map(0xFF474747.toInt(), night = true))
        assertEquals(0xFFAAAAAA.toInt(), GrokTruecolor.map(0xFF606060.toInt(), night = true))
        assertEquals(0xFF5D5D5D.toInt(), GrokTruecolor.map(0xFF8A8A8A.toInt(), night = true))
        assertEquals(0xFFE1E1E1.toInt(), GrokTruecolor.map(0xFFE1E1E1.toInt(), night = true))
    }

    @Test
    fun sameSideAndUnknownColorsStay() {
        assertEquals(0xFF141414.toInt(), GrokTruecolor.map(0xFF141414.toInt(), night = true))
        assertEquals(0xFFEEEEEE.toInt(), GrokTruecolor.map(0xFFEEEEEE.toInt(), night = false))
        assertEquals(0xFF123456.toInt(), GrokTruecolor.map(0xFF123456.toInt(), night = false))
        assertEquals(0xFF123456.toInt(), GrokTruecolor.map(0xFF123456.toInt(), night = true))
    }
}
