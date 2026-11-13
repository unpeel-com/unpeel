package com.unpeel.android

/**
 * Whether the terminal card should shorten for the software keyboard.
 *
 * Same rule as iOS `keyboardOwnsInset`: only a keyboard this terminal
 * summoned moves the card. Show snaps to the animation target. Hide follows
 * the live inset down to zero. A target that stays at the old keyboard height
 * after the keyboard is gone (background, then foreground) must not leave a
 * blank band under the terminal.
 */
internal data class KeyboardInset(
    val ownsInset: Boolean,
    val padPx: Int,
) {
    val open: Boolean get() = padPx > 0
}

internal fun resolveKeyboardInset(
    allow: Boolean,
    ownsInset: Boolean,
    terminalFocused: Boolean,
    imeTargetPx: Int,
    imePx: Int,
): KeyboardInset {
    if (!allow) return KeyboardInset(ownsInset = false, padPx = 0)
    val target = imeTargetPx.coerceAtLeast(0)
    val live = imePx.coerceAtLeast(0)
    // Focus is already gone and the keyboard has left the screen. Ignore a
    // stuck animation target — that is the blank band after a background.
    if (!terminalFocused && live == 0) return KeyboardInset(ownsInset = false, padPx = 0)
    if (target == 0 && live == 0) return KeyboardInset(ownsInset = false, padPx = 0)
    val owned = ownsInset || terminalFocused
    if (!owned) return KeyboardInset(ownsInset = false, padPx = 0)
    val pad = if (target > 0) target else live
    return KeyboardInset(ownsInset = true, padPx = pad)
}
