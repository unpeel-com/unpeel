package com.unpeel.android

import android.content.ContentResolver
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import android.net.Uri
import java.io.ByteArrayOutputStream
import java.io.InputStream
import kotlin.math.max
import kotlin.math.roundToInt

internal sealed class LoadedAttachment {
    class Image(val bitmap: Bitmap) : LoadedAttachment()
    class File(val bytes: ByteArray, val name: String) : LoadedAttachment()
    class Failed(val message: String) : LoadedAttachment()
}

/** Images that decode open as bitmaps. Everything else uploads as a file. */
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
