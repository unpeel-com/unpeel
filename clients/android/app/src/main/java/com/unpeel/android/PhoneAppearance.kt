package com.unpeel.android

/**
 * Phone-local appearance. The Host theme is not read.
 * Theme defaults to the phone. Language defaults to the phone, and a phone
 * language other than Simplified Chinese resolves to English.
 */
internal enum class ThemeChoice {
    System,
    Dark,
    Light,
    ;

    val wire: String
        get() = when (this) {
            System -> "system"
            Dark -> "dark"
            Light -> "light"
        }

    companion object {
        fun from(raw: String?): ThemeChoice = when (raw) {
            "dark" -> Dark
            "light" -> Light
            else -> System
        }
    }
}

internal enum class LanguageChoice {
    System,
    English,
    SimplifiedChinese,
    ;

    val wire: String
        get() = when (this) {
            System -> "system"
            English -> "en"
            SimplifiedChinese -> "zh-Hans"
        }

    companion object {
        fun from(raw: String?): LanguageChoice = when (raw) {
            "en" -> English
            "zh-Hans" -> SimplifiedChinese
            else -> System
        }
    }
}

internal object PhoneAppearance {
    fun night(choice: ThemeChoice, systemNight: Boolean): Boolean = when (choice) {
        ThemeChoice.Dark -> true
        ThemeChoice.Light -> false
        ThemeChoice.System -> systemNight
    }

    fun simplifiedChinese(choice: LanguageChoice, language: String, script: String, country: String): Boolean =
        when (choice) {
            LanguageChoice.English -> false
            LanguageChoice.SimplifiedChinese -> true
            LanguageChoice.System -> systemSimplifiedChinese(language, script, country)
        }

    /** Simplified Chinese only. Traditional (Hant, TW, HK, MO) is unsupported. */
    fun systemSimplifiedChinese(language: String, script: String, country: String): Boolean {
        if (!language.equals("zh", ignoreCase = true)) return false
        if (script.equals("Hant", ignoreCase = true)) return false
        if (script.equals("Hans", ignoreCase = true)) return true
        return !country.equals("TW", ignoreCase = true) &&
            !country.equals("HK", ignoreCase = true) &&
            !country.equals("MO", ignoreCase = true)
    }
}
