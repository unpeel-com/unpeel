package com.unpeel.android

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas as AndroidCanvas
import android.graphics.Paint as AndroidPaint
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathFillType
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.asAndroidPath
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlin.math.ceil

/**
 * The same runtime SVGs iOS draws from the shared catalog. A letter is only
 * the fallback when this Host's command is not a known runtime.
 */
internal data class RuntimeIcon(val asset: String, val ids: Set<String>)

private val RuntimeIcons = listOf(
    RuntimeIcon("amp", setOf("com.sourcegraph.amp", "amp")),
    RuntimeIcon("claude-code", setOf("com.anthropic.claude-code", "claude-code", "claude")),
    RuntimeIcon("cline", setOf("bot.cline.cli", "cline")),
    RuntimeIcon("codex", setOf("com.openai.codex", "codex")),
    RuntimeIcon("cursor-agent", setOf("com.cursor.agent", "cursor-agent", "cursor")),
    RuntimeIcon("fx", setOf("com.vercel.fx", "fx")),
    RuntimeIcon("gemini", setOf("com.google.gemini-cli", "gemini")),
    RuntimeIcon("grok", setOf("ai.x.grok-cli", "grok")),
    RuntimeIcon("kimi", setOf("com.moonshot.kimi-code", "kimi")),
    RuntimeIcon("kiro", setOf("dev.kiro.cli", "kiro", "kiro-cli")),
    RuntimeIcon("muse-code", setOf("ai.meta.muse-code", "muse-code", "muse")),
    RuntimeIcon("omp", setOf("sh.omp.cli", "omp")),
    RuntimeIcon("opencode", setOf("ai.opencode.cli", "opencode")),
    RuntimeIcon("pi", setOf("dev.mariozechner.pi", "pi")),
    RuntimeIcon("agent", setOf("com.github.copilot-cli", "github-copilot", "copilot", "com.google.antigravity-cli", "antigravity", "agy")),
)

internal fun commandToken(command: String): String {
    val word = command.trim().substringBefore(' ').trim('\'', '"')
    return word.substringAfterLast('/').substringAfterLast('\\').lowercase()
}

/** Running sessions prefer the live runtime, the same way iOS picks the row logo. */
internal fun toolIconAsset(
    command: String,
    providerID: String?,
    activeRuntimeID: String?,
    running: Boolean,
): String {
    val live = activeRuntimeID?.takeIf { running && it.isNotBlank() }
    val keys = listOfNotNull(live, providerID?.takeIf { it.isNotBlank() }, commandToken(command))
    for (key in keys) {
        val token = key.trim().lowercase()
        RuntimeIcons.firstOrNull { token in it.ids }?.let { return it.asset }
    }
    return "terminal"
}

/**
 * Authored runtime SVGs ship as white marks (`iconIsTemplate` in the catalog).
 * Tint them with [Foreground] so light chrome is not white-on-white. Multicolor
 * assets (today: OpenCode) keep their original fills.
 */
private val MulticolorToolAssets = setOf("opencode")

@Composable
internal fun ToolLogo(
    command: String,
    providerID: String? = null,
    activeRuntimeID: String? = null,
    running: Boolean = false,
    size: Dp = 22.dp,
) {
    val asset = toolIconAsset(command, providerID, activeRuntimeID, running)
    val tint = if (asset in MulticolorToolAssets) null else Foreground
    if (!SvgIcon(asset, size, tint = tint)) {
        BrandMark(command, size)
    }
}

@Composable
internal fun BranchMark(size: Dp, tint: Color) {
    SvgIcon("branch", size, tint = tint, rotation = 90f)
}

/**
 * Draw a shared SVG asset with an optional tint. White catalog marks are
 * rasterized then color-filtered (iOS `.renderingMode(.template)`), so light
 * mode never paints white-on-white across sidebar, Activity, or presets.
 */
@Composable
internal fun SvgIcon(asset: String, size: Dp, tint: Color? = null, rotation: Float = 0f): Boolean {
    val context = LocalContext.current
    val density = LocalDensity.current
    val picture = remember(asset) { loadSvg(context, asset) } ?: return false
    val ink = tint ?: if (picture.isWhiteTemplate) Foreground else null
    val px = with(density) { ceil(size.toPx()).toInt().coerceAtLeast(1) }
    val bitmap = remember(asset, px) { rasterize(picture, px) } ?: return false
    val tintKey = ink?.toArgb() ?: 0
    key(asset, tintKey, px, rotation) {
        Box(
            Modifier
                .size(size)
                .graphicsLayer { rotationZ = rotation },
        ) {
            Image(
                bitmap = bitmap.asImageBitmap(),
                contentDescription = null,
                modifier = Modifier.size(size),
                colorFilter = ink?.let { ColorFilter.tint(it) },
            )
        }
    }
    return true
}

/** Draw the SVG into a bitmap at [px]×[px]. Template tints use the alpha mask. */
private fun rasterize(picture: SvgPicture, px: Int): Bitmap? {
    if (px <= 0) return null
    val bitmap = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
    val canvas = AndroidCanvas(bitmap)
    val sx = px / picture.width
    val sy = px / picture.height
    canvas.translate(-picture.minX * sx, -picture.minY * sy)
    canvas.scale(sx, sy)
    val fillPaint = AndroidPaint(AndroidPaint.ANTI_ALIAS_FLAG).apply {
        style = AndroidPaint.Style.FILL
    }
    val strokePaint = AndroidPaint(AndroidPaint.ANTI_ALIAS_FLAG).apply {
        style = AndroidPaint.Style.STROKE
    }
    picture.shapes.forEach { shape ->
        val androidPath = android.graphics.Path(shape.path.asAndroidPath())
        shape.fill?.let { fill ->
            fillPaint.color = fill.toArgb()
            canvas.drawPath(androidPath, fillPaint)
        }
        shape.stroke?.let { stroke ->
            strokePaint.color = stroke.toArgb()
            strokePaint.strokeWidth = shape.strokeWidth
            strokePaint.strokeCap = when (shape.cap) {
                StrokeCap.Round -> AndroidPaint.Cap.ROUND
                StrokeCap.Square -> AndroidPaint.Cap.SQUARE
                else -> AndroidPaint.Cap.BUTT
            }
            strokePaint.strokeJoin = when (shape.join) {
                StrokeJoin.Round -> AndroidPaint.Join.ROUND
                StrokeJoin.Bevel -> AndroidPaint.Join.BEVEL
                else -> AndroidPaint.Join.MITER
            }
            canvas.drawPath(androidPath, strokePaint)
        }
    }
    return bitmap
}

private data class SvgPicture(
    val minX: Float,
    val minY: Float,
    val width: Float,
    val height: Float,
    val shapes: List<SvgShape>,
) {
    /** Catalog / chrome SVGs authored as white glyphs for dark chrome. */
    val isWhiteTemplate: Boolean
        get() {
            val paints = shapes.flatMap { listOfNotNull(it.fill, it.stroke) }
            return paints.isNotEmpty() && paints.all(::isNearWhite)
        }
}

private fun isNearWhite(color: Color): Boolean =
    color.alpha > 0.5f && color.red >= 0.90f && color.green >= 0.90f && color.blue >= 0.90f

private data class SvgShape(
    val path: Path,
    val fill: Color?,
    val stroke: Color?,
    val strokeWidth: Float,
    val cap: StrokeCap,
    val join: StrokeJoin,
)

private fun loadSvg(context: Context, asset: String): SvgPicture? =
    runCatching {
        context.assets.open("icons/$asset.svg").use { it.readBytes().decodeToString() }
    }.getOrNull()?.let(::parseSvg)

private fun parseSvg(source: String): SvgPicture? {
    val box = Regex("""viewBox\s*=\s*"([^"]+)"""").find(source)?.groupValues?.get(1)
        ?.split(Regex("\\s+|,")).orEmpty()
    if (box.size != 4) return null
    val minX = box[0].toFloatOrNull() ?: return null
    val minY = box[1].toFloatOrNull() ?: return null
    val width = box[2].toFloatOrNull()?.takeIf { it > 0f } ?: return null
    val height = box[3].toFloatOrNull()?.takeIf { it > 0f } ?: return null
    val root = Regex("""<svg\b([^>]*)>""").find(source)?.groupValues?.get(1).orEmpty()
    val rootAttrs = attributes(root)
    val shapes = Regex("""<path\b([^>]*)/?>""").findAll(source).mapNotNull { match ->
        val attrs = attributes(match.groupValues[1])
        val data = attrs["d"] ?: return@mapNotNull null
        val path = runCatching { PathParser().parsePathString(data).toPath() }.getOrNull()
            ?: return@mapNotNull null
        val evenOdd = (attrs["fill-rule"] ?: rootAttrs["fill-rule"]) == "evenodd"
        path.fillType = if (evenOdd) PathFillType.EvenOdd else PathFillType.NonZero
        val fill = paintColor(attrs["fill"] ?: rootAttrs["fill"], attrs["fill-opacity"] ?: rootAttrs["fill-opacity"])
        val stroke = paintColor(attrs["stroke"] ?: rootAttrs["stroke"], attrs["stroke-opacity"] ?: rootAttrs["stroke-opacity"])
        if (fill == null && stroke == null) return@mapNotNull null
        SvgShape(
            path = path,
            fill = fill,
            stroke = stroke,
            strokeWidth = attrs["stroke-width"]?.toFloatOrNull() ?: 1f,
            cap = if (attrs["stroke-linecap"] == "round") StrokeCap.Round else StrokeCap.Butt,
            join = if (attrs["stroke-linejoin"] == "round") StrokeJoin.Round else StrokeJoin.Miter,
        )
    }.toList()
    if (shapes.isEmpty()) return null
    return SvgPicture(minX, minY, width, height, shapes)
}

private fun attributes(source: String): Map<String, String> =
    Regex("""([\w-]+)\s*=\s*"([^"]*)"""").findAll(source).associate { it.groupValues[1] to it.groupValues[2] }

private fun paintColor(raw: String?, opacity: String?): Color? {
    val value = raw?.trim().orEmpty()
    if (value.isEmpty() || value.equals("none", true) || value.startsWith("url(")) return null
    val color = when {
        value.startsWith("#") -> hexColor(value.removePrefix("#"))
        else -> null
    } ?: return null
    val alpha = opacity?.toFloatOrNull()?.coerceIn(0f, 1f) ?: return color
    return color.copy(alpha = color.alpha * alpha)
}

private fun hexColor(hex: String): Color? = when (hex.length) {
    3 -> Color((hex.flatMap { listOf(it, it) }.joinToString("").toLong(16) or 0xFF000000).toInt())
    6 -> Color((hex.toLong(16) or 0xFF000000).toInt())
    8 -> {
        val rgb = hex.substring(0, 6).toLong(16)
        val alpha = hex.substring(6, 8).toLong(16)
        Color(((alpha shl 24) or rgb).toInt())
    }
    else -> null
}
