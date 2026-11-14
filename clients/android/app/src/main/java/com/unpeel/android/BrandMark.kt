package com.unpeel.android

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Letter mark colored from the runtime tint. The real tool SVGs live in the
 * Apple shared catalog; this is the same identity (command → tint) drawn
 * without a second SVG renderer.
 */
internal data class Brand(val letter: String, val tint: Color)

internal fun brandFor(command: String): Brand {
    val token = command.trim()
        .substringBefore(' ')
        .substringAfterLast('/')
        .substringAfterLast('\\')
        .lowercase()
    val known = KnownBrands.firstOrNull { token == it.token }
    if (known != null) return Brand(known.letter, Color(known.tint.toInt()))
    val letter = token.firstOrNull { it.isLetterOrDigit() }?.uppercaseChar()?.toString() ?: "•"
    return Brand(letter, Color(0xFF8F8787))
}

private data class KnownBrand(val token: String, val letter: String, val tint: Long)

private val KnownBrands = listOf(
    KnownBrand("cursor-agent", "C", 0xFF22C55D),
    KnownBrand("kiro-cli", "K", 0xFFA78BFA),
    KnownBrand("opencode", "O", 0xFF8F8787),
    KnownBrand("gemini", "G", 0xFF6EA8FF),
    KnownBrand("claude", "C", 0xFFD97757),
    KnownBrand("codex", "C", 0xFFC292FE),
    KnownBrand("copilot", "C", 0xFFE6E6E6),
    KnownBrand("kimi", "K", 0xFF8B7CFF),
    KnownBrand("cline", "C", 0xFFF26C5A),
    KnownBrand("muse", "M", 0xFF0082FB),
    KnownBrand("grok", "G", 0xFF8F8787),
    KnownBrand("agy", "A", 0xFF4285F4),
    KnownBrand("amp", "A", 0xFFF97316),
    KnownBrand("omp", "O", 0xFFF97316),
    KnownBrand("pi", "P", 0xFF7C95FF),
    KnownBrand("fx", "F", 0xFFEDEDED),
)

@Composable
internal fun BrandMark(command: String, size: Dp = 22.dp) {
    val brand = brandFor(command)
    // Near-white brand disks (fx / copilot) disappear on light chrome — ink them.
    val disk = readableBrandDisk(brand.tint)
    val light = luminance(disk) > 0.72f
    Box(
        Modifier.size(size).clip(CircleShape).background(disk),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            brand.letter,
            color = if (light) Color(0xFF121314) else Color.White,
            fontSize = (size.value * 0.42f).sp,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

private fun luminance(color: Color): Float =
    0.2126f * color.red + 0.7152f * color.green + 0.0722f * color.blue

@Composable
private fun readableBrandDisk(tint: Color): Color {
    if (phoneNight() || luminance(tint) <= 0.72f) return tint
    return Foreground.copy(alpha = 0.88f)
}
