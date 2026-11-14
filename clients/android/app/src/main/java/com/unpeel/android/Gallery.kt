@file:OptIn(
    androidx.compose.foundation.ExperimentalFoundationApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package com.unpeel.android

import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Paint
import android.graphics.RectF
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import com.unpeel.controller.ArtifactSummary
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.math.atan2
import kotlin.math.cos
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sin
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.offset

/**
 * iOS `BrowserGalleryPanel`. The header has no capture button and no file
 * picker. The first tile is "Add image". A tile opens the detail view.
 * Long-press is Add to message, Open, Delete.
 */
@Composable
internal fun GallerySheet(controller: Controller, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var uploading by remember { mutableStateOf(false) }
    var uploadError by remember { mutableStateOf<String?>(null) }
    var selected by remember { mutableStateOf<ArtifactSummary?>(null) }
    var preview by remember { mutableStateOf<Bitmap?>(null) }
    val thumbs = remember { mutableStateMapOf<String, androidx.compose.ui.graphics.ImageBitmap>() }
    val pickImage = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            uploading = true
            uploadError = null
            val jpeg = withContext(Dispatchers.IO) {
                photoAttachJpeg(context.contentResolver, uri, maxEdge = Int.MAX_VALUE, quality = 90)
            }
            if (jpeg == null) {
                uploadError = "Couldn't read the selected image."
                uploading = false
                return@launch
            }
            controller.addGalleryImage(jpeg) { path, error ->
                uploading = false
                uploadError = error
                if (path == null) return@addGalleryImage
                val name = path.substringAfterLast('/')
                val added = controller.artifacts.firstOrNull { it.kind == "uploads" && it.name == name }
                if (added != null) {
                    preview = BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
                    selected = added
                }
            }
        }
    }
    val sheetHeight = LocalConfiguration.current.screenHeightDp.dp * 0.92f
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState, containerColor = Panel) {
        Column(Modifier.fillMaxWidth().height(sheetHeight).background(Panel)) {
            Column(Modifier.padding(start = 16.dp, end = 16.dp, top = 18.dp, bottom = 10.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        t("Session gallery"),
                        color = Foreground,
                        fontSize = 17.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.weight(1f),
                    )
                    if (uploading) {
                        CircularProgressIndicator(
                            color = Foreground.copy(alpha = 0.7f),
                            modifier = Modifier.size(16.dp),
                            strokeWidth = 2.dp,
                        )
                    }
                }
                Text(
                    t("Screenshots the agent's browser and device capture, plus images you add or mark up."),
                    color = Foreground.copy(alpha = 0.5f),
                    fontSize = 12.5.sp,
                    modifier = Modifier.padding(top = 4.dp),
                )
                uploadError?.let {
                    Text(
                        t(it),
                        color = Danger.copy(alpha = 0.9f),
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Medium,
                        maxLines = 2,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
            }
            if (controller.artifactsLoading && controller.artifacts.isEmpty()) {
                Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = Foreground)
                }
            } else {
                LazyVerticalGrid(
                    columns = GridCells.Fixed(3),
                    modifier = Modifier.fillMaxWidth().weight(1f),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    contentPadding = PaddingValues(start = 12.dp, top = 14.dp, end = 12.dp, bottom = 24.dp),
                ) {
                    item {
                        AddImageTile(enabled = !uploading) {
                            pickImage.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                            )
                        }
                    }
                    items(controller.artifacts, key = { "${it.kind}/${it.name}" }) { artifact ->
                        val key = "${artifact.kind}/${artifact.name}"
                        val imageName = galleryImage(artifact.name)
                        LaunchedEffect(key) {
                            if (!imageName || thumbs.containsKey(key)) return@LaunchedEffect
                            controller.loadGalleryBytes(artifact.kind, artifact.name, 512) { bytes, _ ->
                                if (bytes == null) return@loadGalleryBytes
                                val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return@loadGalleryBytes
                                runCatching { thumbs[key] = bitmap.asImageBitmap() }
                            }
                        }
                        GalleryTile(
                            artifact = artifact,
                            image = thumbs[key],
                            onOpen = {
                                preview = null
                                selected = artifact
                            },
                            onAdd = {
                                controller.pasteArtifact(artifact.kind, artifact.name)
                                onDismiss()
                            },
                            onDelete = {
                                controller.deleteArtifact(artifact.kind, artifact.name) { error ->
                                    uploadError = error
                                }
                                thumbs.remove(key)
                            },
                        )
                    }
                }
            }
        }
    }
    selected?.let { artifact ->
        GalleryDetail(
            controller = controller,
            artifact = artifact,
            preloaded = preview,
            onApply = { bytes, type ->
                controller.pasteImageBytes(bytes, type)
                selected = null
                onDismiss()
            },
            onClose = {
                selected = null
                preview = null
            },
            onFailed = { uploadError = it },
        )
    }
}

@Composable
private fun AddImageTile(enabled: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(10.dp)
    val dash = Foreground.copy(alpha = 0.22f)
    val plus = Foreground.copy(alpha = 0.7f)
    Column(
        Modifier
            .aspectRatio(1f)
            .clip(shape)
            .background(Foreground.copy(alpha = 0.06f))
            .drawBehind {
                drawRoundRect(
                    color = dash,
                    cornerRadius = CornerRadius(10.dp.toPx(), 10.dp.toPx()),
                    style = Stroke(
                        width = 1.dp.toPx(),
                        pathEffect = PathEffect.dashPathEffect(floatArrayOf(5.dp.toPx(), 4.dp.toPx())),
                    ),
                )
            }
            .combinedClickable(enabled = enabled, onClick = onClick),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Canvas(Modifier.size(22.dp)) {
            val paint = plus
            val t = 2.2.dp.toPx()
            drawLine(paint, Offset(size.width / 2f, 2.dp.toPx()), Offset(size.width / 2f, size.height - 2.dp.toPx()), t, StrokeCap.Round)
            drawLine(paint, Offset(2.dp.toPx(), size.height / 2f), Offset(size.width - 2.dp.toPx(), size.height / 2f), t, StrokeCap.Round)
        }
        Text(t("Add image"), color = Foreground.copy(alpha = 0.7f), fontSize = 11.sp, fontWeight = FontWeight.Medium)
    }
}

@Composable
private fun GalleryTile(
    artifact: ArtifactSummary,
    image: androidx.compose.ui.graphics.ImageBitmap?,
    onOpen: () -> Unit,
    onAdd: () -> Unit,
    onDelete: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    val imageName = galleryImage(artifact.name)
    val shape = RoundedCornerShape(10.dp)
    Box(
        Modifier
            .aspectRatio(1f)
            .clip(shape)
            .background(Foreground.copy(alpha = 0.06f))
            .border(1.dp, Foreground.copy(alpha = 0.08f), shape)
            .combinedClickable(onClick = onOpen, onLongClick = { menu = true }),
        contentAlignment = Alignment.Center,
    ) {
        when {
            image != null -> Image(image, contentDescription = artifact.name, modifier = Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
            imageName -> CircularProgressIndicator(color = Foreground.copy(alpha = 0.6f), modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            else -> {
                val ext = artifact.name.substringAfterLast('.', "").uppercase()
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(t("↓"), color = Foreground.copy(alpha = 0.5f), fontSize = 20.sp)
                    if (ext.isNotEmpty()) {
                        Text(ext, color = Foreground.copy(alpha = 0.5f), fontSize = 10.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            if (imageName) {
                DropdownMenuItem(
                    text = { Text(t("Add to message")) },
                    onClick = {
                        menu = false
                        onAdd()
                    },
                )
            }
            DropdownMenuItem(
                text = { Text(t("Open")) },
                onClick = {
                    menu = false
                    onOpen()
                },
            )
            DropdownMenuItem(
                text = { Text(t("Delete"), color = Danger.copy(alpha = 0.9f)) },
                onClick = {
                    menu = false
                    onDelete()
                },
            )
        }
    }
}

@Composable
private fun GalleryDetail(
    controller: Controller,
    artifact: ArtifactSummary,
    preloaded: Bitmap?,
    onApply: (ByteArray, String) -> Unit,
    onClose: () -> Unit,
    onFailed: (String) -> Unit,
) {
    val context = LocalContext.current
    var bitmap by remember(artifact.kind, artifact.name) { mutableStateOf(preloaded) }
    var raw by remember(artifact.kind, artifact.name) { mutableStateOf<ByteArray?>(null) }
    var contentType by remember(artifact.kind, artifact.name) { mutableStateOf("application/octet-stream") }
    var failed by remember(artifact.kind, artifact.name) { mutableStateOf(false) }
    var cropping by remember { mutableStateOf(false) }
    var arrowMode by remember { mutableStateOf(false) }
    var arrows by remember(artifact.kind, artifact.name) { mutableStateOf(listOf<GalleryArrow>()) }
    var arrowColor by remember { mutableStateOf(GalleryArrowPalette[0]) }
    var edited by remember(artifact.kind, artifact.name) { mutableStateOf(false) }
    val imageName = galleryImage(artifact.name)
    LaunchedEffect(artifact.kind, artifact.name) {
        controller.loadGalleryBytes(artifact.kind, artifact.name, null) { bytes, type ->
            if (bytes == null) {
                if (bitmap == null) failed = true
                return@loadGalleryBytes
            }
            raw = bytes
            contentType = type
            val decoded = BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
            if (decoded != null) bitmap = decoded else if (bitmap == null) failed = true
        }
    }
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Column(
            Modifier
                .fillMaxSize()
                .background(Color.Black)
                .windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.statusBars),
        ) {
            Row(
                Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 14.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RoundIconButton("✕", onClose)
                Spacer(Modifier.weight(1f))
                val shareBytes = sharePayload(bitmap, raw, contentType, arrows, artifact.name, edited)
                if (shareBytes != null) {
                    RoundIconButton("↑") {
                        shareGalleryFile(context, shareBytes.first, shareBytes.second, shareBytes.third)
                    }
                }
            }
            Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
                val shown = bitmap
                when {
                    shown != null -> ArrowImage(
                        bitmap = shown,
                        arrows = arrows,
                        arrowMode = arrowMode,
                        color = arrowColor,
                        onArrow = { arrows = arrows + it },
                    )
                    failed -> Text(t("Couldn't load this file"), color = Color.White.copy(alpha = 0.6f), fontSize = 14.sp)
                    else -> CircularProgressIndicator(color = Color.White)
                }
            }
            if (imageName && bitmap != null) {
                if (arrowMode) {
                    ArrowControls(
                        color = arrowColor,
                        onColor = { arrowColor = it },
                        onUndo = { if (arrows.isNotEmpty()) arrows = arrows.dropLast(1) },
                        onClear = { arrows = emptyList() },
                        onDone = { arrowMode = false },
                    )
                } else {
                    Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 10.dp, bottom = 10.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        ToolButton(t("Crop"), Modifier.weight(1f)) { cropping = true }
                        ToolButton(t("Arrows"), Modifier.weight(1f)) { arrowMode = true }
                    }
                }
                val plate = RoundedCornerShape(14.dp)
                Text(
                    t("Add to message"),
                    color = Color.White,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .padding(start = 16.dp, end = 16.dp, bottom = 20.dp)
                        .windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.navigationBars)
                        .fillMaxWidth()
                        .clip(plate)
                        .background(Color.White.copy(alpha = 0.14f))
                        .border(1.dp, Color.White.copy(alpha = 0.14f), plate)
                        .combinedClickable {
                            val payload = attachmentPayload(bitmap, raw, contentType, arrows)
                            if (payload == null) onFailed("Couldn't prepare this image for upload.")
                            else onApply(payload.first, payload.second)
                        }
                        .padding(vertical = 14.dp),
                    textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                )
            }
        }
    }
    val croppingImage = bitmap
    if (cropping && croppingImage != null) {
        Dialog(
            onDismissRequest = { cropping = false },
            properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
        ) {
            GalleryCropper(
                image = if (arrows.isEmpty()) croppingImage else flattenArrows(croppingImage, arrows),
                onCancel = { cropping = false },
                onDone = { cropped ->
                    bitmap = cropped
                    raw = pngBytes(cropped)
                    contentType = "image/png"
                    arrows = emptyList()
                    edited = true
                    cropping = false
                },
            )
        }
    }
}

@Composable
private fun ArrowImage(
    bitmap: Bitmap,
    arrows: List<GalleryArrow>,
    arrowMode: Boolean,
    color: Color,
    onArrow: (GalleryArrow) -> Unit,
) {
    var live by remember { mutableStateOf<GalleryArrow?>(null) }
    var scale by remember { mutableFloatStateOf(1f) }
    var pan by remember { mutableStateOf(Offset.Zero) }
    val transform = rememberTransformableState { zoom, delta, _ ->
        scale = (scale * zoom).coerceIn(1f, 5f)
        pan += delta
    }
    BoxWithConstraints(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        val density = LocalDensity.current
        val maxW = with(density) { maxWidth.toPx() }
        val maxH = with(density) { maxHeight.toPx() }
        val fitted = fitSize(bitmap.width.toFloat(), bitmap.height.toFloat(), maxW, maxH)
        val widthDp = with(density) { fitted.first.toDp() }
        val heightDp = with(density) { fitted.second.toDp() }
        Box(
            Modifier
                .size(widthDp, heightDp)
                .graphicsLayer {
                    scaleX = scale
                    scaleY = scale
                    translationX = pan.x
                    translationY = pan.y
                }
                .then(if (arrowMode) Modifier else Modifier.transformable(transform)),
        ) {
            Image(bitmap.asImageBitmap(), contentDescription = null, modifier = Modifier.fillMaxSize(), contentScale = ContentScale.FillBounds)
            Canvas(
                Modifier
                    .fillMaxSize()
                    .pointerInput(arrowMode, color) {
                        if (!arrowMode) return@pointerInput
                        var start = Offset.Zero
                        detectDragGestures(
                            onDragStart = { start = it },
                            onDrag = { change, _ ->
                                live = GalleryArrow(start.x / size.width, start.y / size.height, change.position.x / size.width, change.position.y / size.height, color)
                            },
                            onDragEnd = {
                                val drawn = live
                                live = null
                                if (drawn != null) {
                                    val dx = (drawn.x1 - drawn.x0) * size.width
                                    val dy = (drawn.y1 - drawn.y0) * size.height
                                    if (hypot(dx.toDouble(), dy.toDouble()) > 8.0) onArrow(drawn)
                                }
                            },
                            onDragCancel = { live = null },
                        )
                    },
            ) {
                val head = max(12f, size.width * 0.038f)
                val stroke = Stroke(width = max(2.5f, size.width * 0.009f), cap = StrokeCap.Round, join = StrokeJoin.Round)
                fun drawArrow(arrow: GalleryArrow) {
                    drawPath(arrowPath(arrow, size.width, size.height, head), arrow.color, style = stroke)
                }
                arrows.forEach(::drawArrow)
                live?.let(::drawArrow)
            }
        }
    }
}

@Composable
private fun ArrowControls(
    color: Color,
    onColor: (Color) -> Unit,
    onUndo: () -> Unit,
    onClear: () -> Unit,
    onDone: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 10.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Text(t("Drag on the image to draw · pinch to zoom"), color = Color.White.copy(alpha = 0.6f), fontSize = 12.sp, fontWeight = FontWeight.Medium)
        Row(Modifier.padding(top = 12.dp), horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            GalleryArrowPalette.forEach { swatch ->
                Box(
                    Modifier
                        .size(28.dp)
                        .clip(CircleShape)
                        .background(swatch)
                        .border(if (swatch == color) 3.dp else 1.dp, Color.White.copy(alpha = if (swatch == color) 0.95f else 0.25f), CircleShape)
                        .combinedClickable { onColor(swatch) },
                )
            }
        }
        Row(Modifier.fillMaxWidth().padding(top = 12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            ToolButton(t("Undo"), Modifier.weight(1f), onUndo)
            ToolButton(t("Clear"), Modifier.weight(1f), onClear)
            ToolButton(t("Done"), Modifier.weight(1f), onDone)
        }
    }
}

@Composable
private fun GalleryCropper(image: Bitmap, onCancel: () -> Unit, onDone: (Bitmap) -> Unit) {
    var crop by remember { mutableStateOf(NormRect(0f, 0f, 1f, 1f)) }
    Box(Modifier.fillMaxSize().background(Color.Black).windowInsetsPadding(androidx.compose.foundation.layout.WindowInsets.statusBars)) {
        BoxWithConstraints(Modifier.fillMaxSize().padding(top = 64.dp, bottom = 24.dp)) {
            val density = LocalDensity.current
            val maxW = constraints.maxWidth.toFloat()
            val maxH = constraints.maxHeight.toFloat()
            val fitted = fitSize(image.width.toFloat(), image.height.toFloat(), maxW, maxH)
            val left = (maxW - fitted.first) / 2f
            val top = (maxH - fitted.second) / 2f
            val handle = with(density) { 28.dp.toPx() }
            val minSide = with(density) { 44.dp.toPx() }
            Image(
                image.asImageBitmap(),
                contentDescription = null,
                modifier = Modifier.offset { IntOffset(left.roundToInt(), top.roundToInt()) }.size(with(density) { fitted.first.toDp() }, with(density) { fitted.second.toDp() }),
                contentScale = ContentScale.FillBounds,
            )
            Canvas(
                Modifier
                    .fillMaxSize()
                    .pointerInput(fitted, left, top) {
                        var mode = CropDrag.Move
                        detectDragGestures(
                            onDragStart = { start ->
                                mode = cropHit(start, crop, left, top, fitted.first, fitted.second, handle)
                            },
                            onDrag = { change, dragAmount ->
                                change.consume()
                                crop = moveCrop(crop, mode, dragAmount.x, dragAmount.y, fitted.first, fitted.second, minSide)
                            },
                        )
                    },
            ) {
                val rect = RectF(
                    left + crop.left * fitted.first,
                    top + crop.top * fitted.second,
                    left + crop.right * fitted.first,
                    top + crop.bottom * fitted.second,
                )
                val hole = androidx.compose.ui.geometry.Size(rect.width(), rect.height())
                drawRect(Color.Black.copy(alpha = 0.55f), size = androidx.compose.ui.geometry.Size(size.width, rect.top.coerceAtLeast(0f)))
                drawRect(Color.Black.copy(alpha = 0.55f), topLeft = Offset(0f, rect.bottom), size = androidx.compose.ui.geometry.Size(size.width, (size.height - rect.bottom).coerceAtLeast(0f)))
                drawRect(Color.Black.copy(alpha = 0.55f), topLeft = Offset(0f, rect.top), size = androidx.compose.ui.geometry.Size(rect.left.coerceAtLeast(0f), hole.height))
                drawRect(Color.Black.copy(alpha = 0.55f), topLeft = Offset(rect.right, rect.top), size = androidx.compose.ui.geometry.Size((size.width - rect.right).coerceAtLeast(0f), hole.height))
                drawRect(Color.White.copy(alpha = 0.9f), topLeft = Offset(rect.left, rect.top), size = hole, style = Stroke(width = 1.5.dp.toPx()))
                listOf(Offset(rect.left, rect.top), Offset(rect.right, rect.top), Offset(rect.left, rect.bottom), Offset(rect.right, rect.bottom)).forEach { point ->
                    drawCircle(Color.White, radius = 8.dp.toPx(), center = point)
                    drawCircle(Color.Black.copy(alpha = 0.25f), radius = 8.dp.toPx(), center = point, style = Stroke(width = 1.dp.toPx()))
                }
            }
        }
        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
            CropCapsule(t("Cancel"), false, onCancel)
            Spacer(Modifier.weight(1f))
            CropCapsule(t("Reset"), false) { crop = NormRect(0f, 0f, 1f, 1f) }
            Spacer(Modifier.width(12.dp))
            CropCapsule(t("Done"), true) { onDone(cropBitmap(image, crop)) }
        }
    }
}

@Composable
private fun CropCapsule(label: String, prominent: Boolean, onClick: () -> Unit) {
    Text(
        label,
        color = if (prominent) Color.Black else Color.White,
        fontSize = 15.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .clip(RoundedCornerShape(20.dp))
            .background(if (prominent) Color.White else Color.White.copy(alpha = 0.14f))
            .combinedClickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 10.dp),
    )
}

@Composable
private fun ToolButton(label: String, modifier: Modifier, onClick: () -> Unit) {
    val shape = RoundedCornerShape(12.dp)
    Text(
        label,
        color = Color.White,
        fontSize = 14.sp,
        fontWeight = FontWeight.SemiBold,
        textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        modifier = modifier
            .clip(shape)
            .background(Color.White.copy(alpha = 0.12f))
            .border(1.dp, Color.White.copy(alpha = 0.12f), shape)
            .combinedClickable(onClick = onClick)
            .padding(vertical = 11.dp),
    )
}

@Composable
private fun RoundIconButton(label: String, onClick: () -> Unit) {
    Box(
        Modifier
            .size(40.dp)
            .clip(CircleShape)
            .background(Color.White.copy(alpha = 0.16f))
            .combinedClickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = Color.White, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
    }
}

private data class NormRect(val left: Float, val top: Float, val right: Float, val bottom: Float)

private enum class CropDrag { Move, TopLeft, TopRight, BottomLeft, BottomRight }

private fun cropHit(point: Offset, crop: NormRect, left: Float, top: Float, width: Float, height: Float, handle: Float): CropDrag {
    fun at(nx: Float, ny: Float) = hypot((point.x - (left + nx * width)).toDouble(), (point.y - (top + ny * height)).toDouble()) <= handle
    return when {
        at(crop.left, crop.top) -> CropDrag.TopLeft
        at(crop.right, crop.top) -> CropDrag.TopRight
        at(crop.left, crop.bottom) -> CropDrag.BottomLeft
        at(crop.right, crop.bottom) -> CropDrag.BottomRight
        else -> CropDrag.Move
    }
}

private fun moveCrop(crop: NormRect, mode: CropDrag, dx: Float, dy: Float, width: Float, height: Float, minSide: Float): NormRect {
    val nx = dx / width
    val ny = dy / height
    val minW = (minSide / width).coerceAtMost(1f)
    val minH = (minSide / height).coerceAtMost(1f)
    return when (mode) {
        CropDrag.Move -> {
            val w = crop.right - crop.left
            val h = crop.bottom - crop.top
            val l = (crop.left + nx).coerceIn(0f, 1f - w)
            val t = (crop.top + ny).coerceIn(0f, 1f - h)
            NormRect(l, t, l + w, t + h)
        }
        CropDrag.TopLeft -> NormRect(
            (crop.left + nx).coerceIn(0f, crop.right - minW),
            (crop.top + ny).coerceIn(0f, crop.bottom - minH),
            crop.right,
            crop.bottom,
        )
        CropDrag.TopRight -> NormRect(
            crop.left,
            (crop.top + ny).coerceIn(0f, crop.bottom - minH),
            (crop.right + nx).coerceIn(crop.left + minW, 1f),
            crop.bottom,
        )
        CropDrag.BottomLeft -> NormRect(
            (crop.left + nx).coerceIn(0f, crop.right - minW),
            crop.top,
            crop.right,
            (crop.bottom + ny).coerceIn(crop.top + minH, 1f),
        )
        CropDrag.BottomRight -> NormRect(
            crop.left,
            crop.top,
            (crop.right + nx).coerceIn(crop.left + minW, 1f),
            (crop.bottom + ny).coerceIn(crop.top + minH, 1f),
        )
    }
}

private fun cropBitmap(image: Bitmap, crop: NormRect): Bitmap {
    val l = (crop.left * image.width).roundToInt().coerceIn(0, image.width - 1)
    val t = (crop.top * image.height).roundToInt().coerceIn(0, image.height - 1)
    val r = (crop.right * image.width).roundToInt().coerceIn(l + 1, image.width)
    val b = (crop.bottom * image.height).roundToInt().coerceIn(t + 1, image.height)
    return Bitmap.createBitmap(image, l, t, r - l, b - t)
}

private data class GalleryArrow(val x0: Float, val y0: Float, val x1: Float, val y1: Float, val color: Color)

private val GalleryArrowPalette = listOf(
    Color(0xFFEF4444),
    Color(0xFFEAB308),
    Color(0xFF22C55E),
    Color(0xFF3B82F6),
    Color.White,
)

private fun arrowPath(arrow: GalleryArrow, width: Float, height: Float, head: Float): Path {
    val start = Offset(arrow.x0 * width, arrow.y0 * height)
    val end = Offset(arrow.x1 * width, arrow.y1 * height)
    val angle = atan2(end.y - start.y, end.x - start.x)
    val spread = Math.PI.toFloat() / 7f
    val left = Offset(end.x - head * cos(angle - spread), end.y - head * sin(angle - spread))
    val right = Offset(end.x - head * cos(angle + spread), end.y - head * sin(angle + spread))
    return Path().apply {
        moveTo(start.x, start.y)
        lineTo(end.x, end.y)
        moveTo(end.x, end.y)
        lineTo(left.x, left.y)
        moveTo(end.x, end.y)
        lineTo(right.x, right.y)
    }
}

private fun flattenArrows(source: Bitmap, arrows: List<GalleryArrow>): Bitmap {
    val out = source.copy(Bitmap.Config.ARGB_8888, true)
    val canvas = android.graphics.Canvas(out)
    val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        strokeWidth = max(2.5f, out.width * 0.009f)
    }
    val head = max(12f, out.width * 0.038f)
    arrows.forEach { arrow ->
        paint.color = arrow.color.toArgb()
        val path = android.graphics.Path()
        val startX = arrow.x0 * out.width
        val startY = arrow.y0 * out.height
        val endX = arrow.x1 * out.width
        val endY = arrow.y1 * out.height
        val angle = atan2(endY - startY, endX - startX)
        val spread = Math.PI.toFloat() / 7f
        path.moveTo(startX, startY)
        path.lineTo(endX, endY)
        path.moveTo(endX, endY)
        path.lineTo(endX - head * cos(angle - spread), endY - head * sin(angle - spread))
        path.moveTo(endX, endY)
        path.lineTo(endX - head * cos(angle + spread), endY - head * sin(angle + spread))
        canvas.drawPath(path, paint)
    }
    return out
}

private fun attachmentPayload(bitmap: Bitmap?, raw: ByteArray?, contentType: String, arrows: List<GalleryArrow>): Pair<ByteArray, String>? {
    if (bitmap != null && arrows.isNotEmpty()) return pngBytes(flattenArrows(bitmap, arrows)) to "image/png"
    if (raw != null) return raw to contentType
    if (bitmap != null) return pngBytes(bitmap) to "image/png"
    return null
}

private fun sharePayload(
    bitmap: Bitmap?,
    raw: ByteArray?,
    contentType: String,
    arrows: List<GalleryArrow>,
    name: String,
    edited: Boolean,
): Triple<ByteArray, String, String>? {
    val payload = attachmentPayload(bitmap, raw, contentType, arrows) ?: return null
    val fileName = if (arrows.isNotEmpty() || edited) annotatedName(name) else name
    val mime = payload.second.ifEmpty { "application/octet-stream" }
    return Triple(payload.first, fileName, mime)
}

private fun annotatedName(name: String): String {
    val dot = name.lastIndexOf('.')
    val base = if (dot > 0) name.substring(0, dot) else name
    return "$base-annotated.png"
}

private fun pngBytes(bitmap: Bitmap): ByteArray {
    val out = ByteArrayOutputStream()
    bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
    return out.toByteArray()
}

private fun fitSize(imageW: Float, imageH: Float, boundsW: Float, boundsH: Float): Pair<Float, Float> {
    if (imageW <= 0f || imageH <= 0f || boundsW <= 0f || boundsH <= 0f) return 1f to 1f
    val scale = min(boundsW / imageW, boundsH / imageH)
    return imageW * scale to imageH * scale
}

private fun shareGalleryFile(context: android.content.Context, bytes: ByteArray, name: String, mime: String) {
    val safe = name.substringAfterLast('/').filter { it.isLetterOrDigit() || it == '.' || it == '-' || it == '_' }.ifEmpty { "image" }
    val dir = File(context.cacheDir, "gallery").apply { mkdirs() }
    val file = File(dir, safe)
    file.writeBytes(bytes)
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", file)
    val send = Intent(Intent.ACTION_SEND).apply {
        type = mime
        putExtra(Intent.EXTRA_STREAM, uri)
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
    context.startActivity(Intent.createChooser(send, null))
}

/** iOS gallery `isImage`: png, jpg, jpeg, gif, webp. */
internal fun galleryImage(name: String): Boolean {
    val lower = name.lowercase()
    return lower.endsWith(".png") || lower.endsWith(".jpg") || lower.endsWith(".jpeg") ||
        lower.endsWith(".gif") || lower.endsWith(".webp")
}
