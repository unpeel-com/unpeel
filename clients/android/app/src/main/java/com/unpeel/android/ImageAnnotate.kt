package com.unpeel.android

import android.content.ContentResolver
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.PorterDuff
import android.graphics.PorterDuffXfermode
import android.media.ExifInterface
import android.net.Uri
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.dp
import java.io.ByteArrayOutputStream
import java.io.InputStream
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

internal sealed class LoadedAttachment {
    class Image(val bitmap: Bitmap) : LoadedAttachment()
    class File(val bytes: ByteArray, val name: String) : LoadedAttachment()
    class Failed(val message: String) : LoadedAttachment()
}

/** Images that decode open the annotator. Everything else uploads as a file. */
internal fun loadAttachment(context: android.content.Context, uri: android.net.Uri): LoadedAttachment {
    val resolver = context.contentResolver
    val name = resolver.query(uri, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
        if (cursor.moveToFirst()) cursor.getString(0) else null
    }?.takeIf { it.isNotBlank() } ?: "file"
    val mime = resolver.getType(uri).orEmpty()
    val bytes = try {
        resolver.openInputStream(uri)?.use { readBounded(it, 64 * 1024 * 1024) }
            ?: return LoadedAttachment.Failed("Couldn’t open that file")
    } catch (_: AttachmentTooLarge) {
        return LoadedAttachment.Failed("File is over 64 MB")
    } catch (_: Exception) {
        return LoadedAttachment.Failed("Couldn’t open that file")
    }
    if (bytes.isEmpty()) return LoadedAttachment.Failed("That file is empty")
    if (mime.startsWith("image/") && bytes.size <= 20 * 1024 * 1024) {
        val bitmap = decodeUploadBitmap(java.io.ByteArrayInputStream(bytes))
        if (bitmap != null) return LoadedAttachment.Image(bitmap)
    }
    return LoadedAttachment.File(bytes, name)
}

private class AttachmentTooLarge : Exception()

private fun readBounded(source: InputStream, maxBytes: Int): ByteArray {
    val out = ByteArrayOutputStream()
    val buffer = ByteArray(16 * 1024)
    var total = 0
    while (true) {
        val count = source.read(buffer)
        if (count < 0) break
        total += count
        if (total > maxBytes) throw AttachmentTooLarge()
        out.write(buffer, 0, count)
    }
    return out.toByteArray()
}

internal fun decodeUploadBitmap(source: InputStream): Bitmap? {
    val bytes = source.readBytes()
    if (bytes.isEmpty()) return null
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    var sample = 1
    val longEdge = maxOf(bounds.outWidth, bounds.outHeight)
    while (longEdge / sample > 1600) sample *= 2
    val opts = BitmapFactory.Options().apply { inSampleSize = sample }
    return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
}

/**
 * iOS accessory `compressedJPEG`: longest edge 2048, JPEG quality 0.85.
 * EXIF orientation is baked in, the same way `UIImage.draw` does.
 */
internal fun photoAttachJpeg(
    resolver: ContentResolver,
    uri: Uri,
    maxEdge: Int = 2048,
    quality: Int = 85,
): ByteArray? {
    val bytes = try {
        resolver.openInputStream(uri)?.use { readBounded(it, 40 * 1024 * 1024) }
            ?: return null
    } catch (_: Exception) {
        return null
    }
    if (bytes.isEmpty()) return null
    val orientation = runCatching {
        ExifInterface(java.io.ByteArrayInputStream(bytes)).getAttributeInt(
            ExifInterface.TAG_ORIENTATION,
            ExifInterface.ORIENTATION_NORMAL,
        )
    }.getOrDefault(ExifInterface.ORIENTATION_NORMAL)
    val decoded = decodeForPhotoAttach(bytes, maxEdge) ?: return null
    val oriented = applyExifOrientation(decoded, orientation)
    val largest = max(oriented.width, oriented.height)
    val output = if (largest > maxEdge) {
        val scale = maxEdge.toFloat() / largest
        Bitmap.createScaledBitmap(
            oriented,
            (oriented.width * scale).roundToInt().coerceAtLeast(1),
            (oriented.height * scale).roundToInt().coerceAtLeast(1),
            true,
        )
    } else {
        oriented
    }
    val jpeg = output.jpeg(quality)
    if (output !== oriented) output.recycle()
    if (oriented !== decoded) oriented.recycle()
    decoded.recycle()
    return jpeg
}

private fun decodeForPhotoAttach(bytes: ByteArray, maxEdge: Int): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    var sample = 1
    val longEdge = max(bounds.outWidth, bounds.outHeight)
    while (maxEdge < Int.MAX_VALUE / 2 && longEdge / (sample * 2) > maxEdge) sample *= 2
    val opts = BitmapFactory.Options().apply { inSampleSize = sample }
    return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
}

private fun applyExifOrientation(bitmap: Bitmap, orientation: Int): Bitmap {
    val matrix = Matrix()
    when (orientation) {
        ExifInterface.ORIENTATION_ROTATE_90 -> matrix.postRotate(90f)
        ExifInterface.ORIENTATION_ROTATE_180 -> matrix.postRotate(180f)
        ExifInterface.ORIENTATION_ROTATE_270 -> matrix.postRotate(270f)
        ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> matrix.postScale(-1f, 1f)
        ExifInterface.ORIENTATION_FLIP_VERTICAL -> matrix.postScale(1f, -1f)
        ExifInterface.ORIENTATION_TRANSPOSE -> {
            matrix.postRotate(90f)
            matrix.postScale(-1f, 1f)
        }
        ExifInterface.ORIENTATION_TRANSVERSE -> {
            matrix.postRotate(270f)
            matrix.postScale(-1f, 1f)
        }
        else -> return bitmap
    }
    return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
}

/** JPEG small enough for a one-shot Link frame (512 KiB sealed, body is base64). */
internal fun compressJpeg(bitmap: Bitmap, maxBytes: Int): ByteArray {
    var image = bitmap
    var quality = 80
    var bytes = image.jpeg(quality)
    while (bytes.size > maxBytes && quality > 45) {
        quality -= 8
        bytes = image.jpeg(quality)
    }
    var guard = 0
    while (bytes.size > maxBytes && image.width > 360 && guard < 6) {
        val scaled = Bitmap.createScaledBitmap(
            image,
            (image.width * 0.75f).toInt().coerceAtLeast(1),
            (image.height * 0.75f).toInt().coerceAtLeast(1),
            true,
        )
        if (image !== bitmap) image.recycle()
        image = scaled
        quality = 70
        bytes = image.jpeg(quality)
        guard += 1
    }
    if (image !== bitmap) image.recycle()
    return bytes
}

private fun Bitmap.jpeg(quality: Int): ByteArray {
    val stream = ByteArrayOutputStream()
    compress(Bitmap.CompressFormat.JPEG, quality, stream)
    return stream.toByteArray()
}

private data class Ink(val points: List<Offset>, val color: Color, val erase: Boolean)

private data class Fitted(val left: Float, val top: Float, val width: Float, val height: Float)

private fun fitRect(view: Size, source: Bitmap): Fitted {
    val scale = min(view.width / source.width, view.height / source.height)
    val width = source.width * scale
    val height = source.height * scale
    return Fitted((view.width - width) / 2f, (view.height - height) / 2f, width, height)
}

private fun imagePoint(offset: Offset, view: Size, source: Bitmap): Offset? {
    val fitted = fitRect(view, source)
    if (fitted.width <= 0f || fitted.height <= 0f) return null
    val x = (offset.x - fitted.left) / fitted.width
    val y = (offset.y - fitted.top) / fitted.height
    if (x !in 0f..1f || y !in 0f..1f) return null
    return Offset(x, y)
}

private fun strokePath(points: List<Offset>): Path = Path().apply {
    points.forEachIndexed { index, point ->
        if (index == 0) moveTo(point.x, point.y) else lineTo(point.x, point.y)
    }
}

private fun flattenAnnotation(source: Bitmap, strokes: List<Ink>): Bitmap {
    val overlay = Bitmap.createBitmap(source.width, source.height, Bitmap.Config.ARGB_8888)
    val overlayCanvas = android.graphics.Canvas(overlay)
    val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }
    strokes.forEach { stroke ->
        paint.strokeWidth = (if (stroke.erase) 36f else 7f) * source.width / 360f
        paint.color = stroke.color.toArgb()
        paint.xfermode = if (stroke.erase) PorterDuffXfermode(PorterDuff.Mode.CLEAR) else null
        val path = android.graphics.Path()
        stroke.points.forEachIndexed { index, point ->
            val x = point.x * source.width
            val y = point.y * source.height
            if (index == 0) path.moveTo(x, y) else path.lineTo(x, y)
        }
        if (stroke.points.size == 1) {
            val point = stroke.points[0]
            overlayCanvas.drawCircle(point.x * source.width, point.y * source.height, paint.strokeWidth / 2f, paint)
        } else {
            overlayCanvas.drawPath(path, paint)
        }
    }
    val out = Bitmap.createBitmap(source.width, source.height, Bitmap.Config.ARGB_8888)
    val canvas = android.graphics.Canvas(out)
    canvas.drawColor(android.graphics.Color.WHITE)
    canvas.drawBitmap(source, 0f, 0f, null)
    canvas.drawBitmap(overlay, 0f, 0f, null)
    overlay.recycle()
    return out
}

@Composable
internal fun ImageAnnotator(source: Bitmap, onCancel: () -> Unit, onDone: (Bitmap) -> Unit) {
    val strokes = remember { mutableListOf<Ink>() }
    var current by remember { mutableStateOf<List<Offset>>(emptyList()) }
    var color by remember { mutableStateOf(Color.White) }
    var erase by remember { mutableStateOf(false) }
    var tick by remember { mutableIntStateOf(0) }
    val palette = listOf(Color.White, Color(0xFFE85D5D), Color(0xFFF5C16C), Color(0xFF7CB7FF), Color(0xFF7DDE92))
    Column(Modifier.fillMaxSize().background(HeaderInk)) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = onCancel) { Text(t("Cancel")) }
            TextButton(onClick = {
                if (strokes.isNotEmpty()) strokes.removeAt(strokes.lastIndex)
                tick += 1
            }) { Text(t("Undo")) }
            TextButton(onClick = {
                strokes.clear()
                current = emptyList()
                tick += 1
            }) { Text(t("Clear")) }
            Box(Modifier.weight(1f))
            TextButton(onClick = { onDone(flattenAnnotation(source, strokes.toList())) }) { Text(t("Done")) }
        }
        Box(Modifier.weight(1f).fillMaxWidth()) {
            Image(
                bitmap = source.asImageBitmap(),
                contentDescription = null,
                modifier = Modifier.fillMaxSize(),
                contentScale = ContentScale.Fit,
            )
            Canvas(
                Modifier
                    .fillMaxSize()
                    .pointerInput(source) {
                        detectDragGestures(
                            onDragStart = { offset ->
                                val point = imagePoint(offset, Size(size.width.toFloat(), size.height.toFloat()), source)
                                current = if (point == null) emptyList() else listOf(point)
                                tick += 1
                            },
                            onDrag = { change, _ ->
                                val point = imagePoint(change.position, Size(size.width.toFloat(), size.height.toFloat()), source)
                                if (point != null && current.isNotEmpty()) {
                                    current = current + point
                                    tick += 1
                                }
                            },
                            onDragEnd = {
                                if (current.isNotEmpty()) strokes.add(Ink(current, color, erase))
                                current = emptyList()
                                tick += 1
                            },
                            onDragCancel = {
                                current = emptyList()
                                tick += 1
                            },
                        )
                    },
            ) {
                if (tick < 0) return@Canvas
                val fitted = fitRect(size, source)
                val live = if (current.isEmpty()) strokes.toList() else strokes + Ink(current, color, erase)
                live.forEach { stroke ->
                    val paint = if (stroke.erase) Color.Black.copy(alpha = 0.45f) else stroke.color
                    val width = if (stroke.erase) 28.dp.toPx() else 4.dp.toPx()
                    val mapped = stroke.points.map { Offset(fitted.left + it.x * fitted.width, fitted.top + it.y * fitted.height) }
                    if (mapped.size == 1) {
                        drawCircle(paint, radius = width / 2f, center = mapped[0])
                    } else if (mapped.size > 1) {
                        drawPath(strokePath(mapped), paint, style = Stroke(width = width, cap = StrokeCap.Round))
                    }
                }
            }
        }
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            palette.forEach { swatch ->
                val selected = !erase && swatch == color
                Box(
                    Modifier
                        .size(if (selected) 28.dp else 22.dp)
                        .clip(CircleShape)
                        .background(swatch)
                        .clickable {
                            color = swatch
                            erase = false
                        },
                )
            }
            Text(
                "Eraser",
                color = if (erase) Color(0xFF121314) else Foreground,
                modifier = Modifier
                    .clip(CircleShape)
                    .background(if (erase) Color.White else Foreground.copy(alpha = 0.12f))
                    .clickable { erase = true }
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            )
        }
    }
}
