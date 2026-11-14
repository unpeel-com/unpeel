package com.unpeel.android

import android.content.Context
import android.content.res.Configuration
import android.os.Build
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.util.Locale

/**
 * Controller-local theme and language. Stored in the same `unpeel` prefs as
 * the app lock. Not written to the Host and not read from Host appearance.
 */
internal class PhoneSettings(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    var theme by mutableStateOf(ThemeChoice.from(prefs.getString(THEME, null)))
        private set

    var language by mutableStateOf(LanguageChoice.from(prefs.getString(LANGUAGE, null)))
        private set

    fun chooseTheme(value: ThemeChoice) {
        theme = value
        prefs.edit().putString(THEME, value.wire).apply()
    }

    fun chooseLanguage(value: LanguageChoice) {
        language = value
        prefs.edit().putString(LANGUAGE, value.wire).apply()
    }

    companion object {
        private const val PREFS = "unpeel"
        private const val THEME = "unpeel.android.theme"
        private const val LANGUAGE = "unpeel.android.language"

        fun storedNight(context: Context, systemNight: Boolean): Boolean {
            val raw = context.applicationContext
                .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .getString(THEME, null)
            return PhoneAppearance.night(ThemeChoice.from(raw), systemNight)
        }

        fun usesSimplifiedChinese(context: Context): Boolean {
            val raw = context.applicationContext
                .getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .getString(LANGUAGE, null)
            val locale = systemLocale(context.applicationContext)
            return PhoneAppearance.simplifiedChinese(
                LanguageChoice.from(raw),
                locale.language,
                locale.script,
                locale.country,
            )
        }

        fun systemLocale(context: Context): Locale {
            val config = context.resources.configuration
            return if (Build.VERSION.SDK_INT >= 24) {
                config.locales[0]
            } else {
                @Suppress("DEPRECATION")
                config.locale
            }
        }

        fun systemNight(context: Context): Boolean =
            (context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
                Configuration.UI_MODE_NIGHT_YES
    }
}

internal val LocalPhoneSettings = compositionLocalOf<PhoneSettings?> { null }
