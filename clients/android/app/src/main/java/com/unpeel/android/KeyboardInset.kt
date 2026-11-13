package com.unpeel.android

/**
 * Whether the terminal card should shorten for the software keyboard.
 *
 * Only a keyboard this terminal summoned moves the card. The pad is the
 * inset actually on screen, so the card moves with the keyboard instead of
 * jumping to the final height at the start or keeping a blank band after
 * the keyboard has already left. A target that stays at the old height
 * (background, then foreground) is not a pad.
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
    if (live == 0 && (target == 0 || !terminalFocused)) {
        return KeyboardInset(ownsInset = false, padPx = 0)
    }
    val owned = ownsInset || terminalFocused
    if (!owned) return KeyboardInset(ownsInset = false, padPx = 0)
    return KeyboardInset(ownsInset = true, padPx = live)
}
