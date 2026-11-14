package com.unpeel.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneAppearanceTest {
    @Test
    fun themeFollowsTheChoice() {
        assertTrue(PhoneAppearance.night(ThemeChoice.Dark, systemNight = false))
        assertFalse(PhoneAppearance.night(ThemeChoice.Light, systemNight = true))
        assertTrue(PhoneAppearance.night(ThemeChoice.System, systemNight = true))
        assertFalse(PhoneAppearance.night(ThemeChoice.System, systemNight = false))
    }

    @Test
    fun unknownThemeWireIsSystem() {
        assertEquals(ThemeChoice.System, ThemeChoice.from(null))
        assertEquals(ThemeChoice.System, ThemeChoice.from("nope"))
        assertEquals(ThemeChoice.Dark, ThemeChoice.from("dark"))
        assertEquals(ThemeChoice.Light, ThemeChoice.from("light"))
    }

    @Test
    fun languageChoiceOverridesThePhone() {
        assertFalse(PhoneAppearance.simplifiedChinese(LanguageChoice.English, "zh", "Hans", "CN"))
        assertTrue(PhoneAppearance.simplifiedChinese(LanguageChoice.SimplifiedChinese, "en", "", "US"))
    }

    @Test
    fun systemLanguageFallsBackToEnglish() {
        assertTrue(PhoneAppearance.systemSimplifiedChinese("zh", "Hans", "CN"))
        assertTrue(PhoneAppearance.systemSimplifiedChinese("zh", "", "CN"))
        assertTrue(PhoneAppearance.systemSimplifiedChinese("zh", "", "SG"))
        assertTrue(PhoneAppearance.systemSimplifiedChinese("zh", "", ""))
        assertFalse(PhoneAppearance.systemSimplifiedChinese("zh", "Hant", "TW"))
        assertFalse(PhoneAppearance.systemSimplifiedChinese("zh", "", "TW"))
        assertFalse(PhoneAppearance.systemSimplifiedChinese("zh", "", "HK"))
        assertFalse(PhoneAppearance.systemSimplifiedChinese("zh", "", "MO"))
        assertFalse(PhoneAppearance.systemSimplifiedChinese("en", "", "US"))
        assertFalse(PhoneAppearance.systemSimplifiedChinese("ja", "", "JP"))
        assertFalse(PhoneAppearance.simplifiedChinese(LanguageChoice.System, "fr", "", "FR"))
    }
}
