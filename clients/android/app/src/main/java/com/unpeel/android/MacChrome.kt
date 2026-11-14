package com.unpeel.android

import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.graphics.Color

/**
 * Non-terminal chrome from Mac `Theme.swift`.
 * Pairs are the values written there (`Theme.foreground`, `appBackground`,
 * `terminalBackground`, `hoverRow`, `activeRow`, `resizerLine`,
 * `paneDividerLine`, `genericSpinner`, `ProjectFolderColor.nsColor`).
 * Attention, unread, danger, and accent are one hex in both appearances.
 * iOS sheet `#1B1C22` and panel `#16171C` are not in that table. Dark keeps
 * those shipped values. Light uses Mac surface `#FFFFFF`.
 */
internal object MacChrome {
    fun appBackground(night: Boolean): Int = opaque(if (night) 0x121314 else 0xFFFFFF)
    fun surface(night: Boolean): Int = opaque(if (night) 0x1A1B1D else 0xFFFFFF)
    fun sheet(night: Boolean): Int = if (night) opaque(0x1B1C22) else surface(false)
    fun panel(night: Boolean): Int = if (night) opaque(0x16171C) else surface(false)
    fun foreground(night: Boolean): Int = opaque(if (night) 0xF3F5FB else 0x111217)

    /** Foreground @ 0.66 dark / 0.60 light. */
    fun muted(night: Boolean): Int = if (night) 0xA8F3F5FB.toInt() else 0x99111217.toInt()

    /** Foreground @ 0.10. Byte `0x1A` is the same 0.10 the dark chrome already used. */
    fun hover(night: Boolean): Int = if (night) 0x1AF3F5FB else 0x1A111217

    /** Light solid white. Dark white @ 0.16 (`0x29`). */
    fun activeRow(night: Boolean): Int = if (night) 0x29FFFFFF else opaque(0xFFFFFF)

    /** `resizerLine`: black @ 0.08 light, white @ 0.055 dark. */
    fun resizer(night: Boolean): Int = if (night) 0x0EFFFFFF else 0x14000000

    /** `paneDividerLine`: black @ 0.10 light, white @ 0.14 dark. */
    fun paneDivider(night: Boolean): Int = if (night) 0x24FFFFFF else 0x1A000000

    fun spinner(night: Boolean): Int = opaque(if (night) 0xB9BDC9 else 0x4A4D55)

    /** Existing card wash, 0.06, on the foreground ink. */
    fun softCard(night: Boolean): Int = if (night) 0x0FF3F5FB else 0x0F111217

    /** `ProjectFolderColor.nsColor` light / dark. */
    fun folder(id: String?, night: Boolean): Int? {
        val rgb = when (id) {
            "sky" -> if (night) 0x7DD3FC else 0x2095C9
            "blue" -> if (night) 0x7EA6FF else 0x4F73E6
            "violet" -> if (night) 0xB79CFF else 0x7B5BDA
            "rose" -> if (night) 0xF79AC0 else 0xD75F8F
            "amber" -> if (night) 0xF8C86A else 0xB87511
            "moss" -> if (night) 0x9DD67A else 0x5F9A3D
            "teal" -> if (night) 0x64DCCB else 0x159B91
            "graphite" -> if (night) 0xB8BCC8 else 0x687083
            else -> return null
        }
        return opaque(rgb)
    }

    private fun opaque(rgb: Int): Int = 0xFF000000.toInt() or (rgb and 0xFFFFFF)
}

internal val HeaderInk: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.appBackground(phoneNight()))

internal val Canvas: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.surface(phoneNight()))

internal val Sheet: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.sheet(phoneNight()))

internal val Panel: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.panel(phoneNight()))

internal val Foreground: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.foreground(phoneNight()))

internal val Muted: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.muted(phoneNight()))

internal val Hover: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.hover(phoneNight()))

internal val ActiveRow: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.activeRow(phoneNight()))

internal val Hairline: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.resizer(phoneNight()))

internal val SheetHairline: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.paneDivider(phoneNight()))

internal val SoftCard: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.softCard(phoneNight()))

internal val GenericSpinner: Color
    @Composable @ReadOnlyComposable get() = Color(MacChrome.spinner(phoneNight()))

internal val KeyFill: Color
    @Composable @ReadOnlyComposable get() = Hover

/** `Theme.attention`. Same hex in both appearances. */
internal val Attention = Color(0xFFF59E0B)

/** `Theme.unread`. */
internal val Unread = Color(0xFF60A5FA)

/** `Theme.danger`. */
internal val Danger = Color(0xFFEF4444)

/** `Theme.accent`. */
internal val Accent = Color(0xFF34C759)

@Composable
@ReadOnlyComposable
internal fun folderSwatch(id: String?): Color? =
    MacChrome.folder(id, phoneNight())?.let { Color(it) }
