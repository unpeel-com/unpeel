@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class, androidx.compose.material3.ExperimentalMaterial3Api::class)

package com.unpeel.android

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import com.unpeel.controller.PendingApproval
import com.unpeel.controller.ProjectSummary
import com.unpeel.controller.SessionSummary
import com.unpeel.controller.TerminalInput
import com.unpeel.controller.VtScreen
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.roundToInt

private val Frame = Color(0xFF121314)
private val Canvas = Color(0xFF1A1B1D)
private val Sheet = Color(0xFF1B1C22)
private val Foreground = Color(0xFFF3F5FB)
private val Muted = Color(0xA8F3F5FB)
private val Hover = Color(0x1AF3F5FB)
private val ActiveRow = Color(0x29FFFFFF)
private val Attention = Color(0xFFF59E0B)
private val Unread = Color(0xFF60A5FA)
private val Hairline = Color(0x1AFFFFFF)
private val KeyFill = Color(0x1AFFFFFF)

private val SpinnerFrames = listOf("⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏")

private enum class HomeSheet { Workspaces, Activity, Gallery, Organize, Presets }

@Composable
fun UnpeelApp(controller: Controller) {
    MaterialTheme(colorScheme = darkColorScheme(background = Frame, surface = Sheet, primary = Foreground)) {
        Box(Modifier.fillMaxSize().background(Frame).windowInsetsPadding(WindowInsets.statusBars)) {
            if (controller.selectedHost == null) {
                PairScreen(controller, firstRun = true)
            } else {
                TerminalHome(controller)
            }
        }
    }
}

@Composable
private fun TerminalHome(controller: Controller) {
    var sheet by remember { mutableStateOf<HomeSheet?>(null) }
    var presetProject by remember { mutableStateOf<ProjectSummary?>(null) }
    var organizeID by remember { mutableStateOf<String?>(null) }
    var confirmingRemove by remember { mutableStateOf(false) }
    var annotate by remember { mutableStateOf<Bitmap?>(null) }
    var drawerPosition by remember { mutableFloatStateOf(0f) }
    var drawerDragging by remember { mutableStateOf(false) }
    var drawerOpen by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val session = controller.snapshot?.sessions?.firstOrNull { it.id == controller.openSessionID }
    val pickImage = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val bitmap = withContext(Dispatchers.IO) {
                context.contentResolver.openInputStream(uri)?.use { decodeUploadBitmap(it) }
            }
            if (bitmap == null) controller.reportError("打不开这张图片") else annotate = bitmap
        }
    }
    fun chooseImage() {
        pickImage.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
    }
    LaunchedEffect(controller.openSessionID) {
        if (controller.openSessionID != null) {
            drawerDragging = false
            drawerOpen = false
        }
    }
    Box(Modifier.fillMaxSize().background(Frame)) {
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val drawerWidth = minOf(360.dp, maxOf(286.dp, maxWidth * 0.88f))
        val drawerPx = with(LocalDensity.current) { drawerWidth.toPx() }.coerceAtLeast(1f)
        LaunchedEffect(drawerOpen, drawerDragging, drawerPx) {
            if (drawerDragging) return@LaunchedEffect
            val target = if (drawerOpen) drawerPx else 0f
            if (drawerPosition == target) return@LaunchedEffect
            animate(drawerPosition, target, animationSpec = tween(220)) { value, _ -> drawerPosition = value }
        }
        fun dragBy(amount: Float) {
            drawerDragging = true
            drawerPosition = (drawerPosition + amount).coerceIn(0f, drawerPx)
        }
        fun releaseDrawer() {
            drawerOpen = drawerPosition > drawerPx * 0.4f
            drawerDragging = false
        }
        Box(Modifier.fillMaxSize()) {
            if (session == null) {
                EmptyTerminal(disconnected = controller.snapshot == null, hostName = controller.selectedHost?.macName) {
                    drawerOpen = true
                }
            } else {
                TerminalSurface(controller, session, onSessions = { drawerOpen = true }, onPickImage = ::chooseImage, onSheet = { kind ->
                    if (kind == HomeSheet.Organize) organizeID = session.id
                    sheet = kind
                })
            }
            Box(
                Modifier
                    .fillMaxHeight()
                    .width(28.dp)
                    .pointerInput(drawerPx) {
                        detectHorizontalDragGestures(
                            onDragStart = { drawerDragging = true },
                            onHorizontalDrag = { _, amount -> dragBy(amount) },
                            onDragEnd = { releaseDrawer() },
                            onDragCancel = { releaseDrawer() },
                        )
                    },
            )
            val progress = (drawerPosition / drawerPx).coerceIn(0f, 1f)
            if (progress > 0f) {
                Box(
                    Modifier
                        .fillMaxSize()
                        .background(Color.Black.copy(alpha = 0.38f * progress))
                        .then(
                            if (drawerDragging) {
                                Modifier
                            } else {
                                Modifier.clickable {
                                    drawerDragging = false
                                    drawerOpen = false
                                }
                            },
                        ),
                )
            }
            Box(Modifier.offset { IntOffset((drawerPosition - drawerPx).roundToInt(), 0) }) {
                SessionDrawer(
                    controller = controller,
                    selectedID = controller.openSessionID,
                    width = drawerWidth,
                    onDrag = ::dragBy,
                    onRelease = ::releaseDrawer,
                    onClose = {
                        drawerDragging = false
                        drawerOpen = false
                    },
                    onWorkspaces = { sheet = HomeSheet.Workspaces },
                    onNewSession = { project ->
                        presetProject = project
                        sheet = HomeSheet.Presets
                    },
                    onOrganize = { chosen ->
                        drawerDragging = false
                        drawerOpen = false
                        organizeID = chosen.id
                        sheet = HomeSheet.Organize
                    },
                )
            }
        }
    }
    val drawerMostlyOpen = drawerOpen || drawerPosition > 80f
    val approval = session?.let { current ->
        controller.snapshot?.pendingApprovals?.firstOrNull {
            it.targetSessionID == current.id || it.callerSessionID == current.id
        }
    }
    if (approval != null && !drawerMostlyOpen) {
        ApprovalOverlay(approval, more = (controller.snapshot?.pendingApprovals?.count {
            it.targetSessionID == session.id || it.callerSessionID == session.id
        } ?: 1) - 1, onAnswer = controller::answer)
    }
    when (sheet) {
        HomeSheet.Workspaces -> HostSheet(controller) { sheet = null }
        HomeSheet.Activity -> ActivitySheet(
            controller,
            onDismiss = { sheet = null },
            onSelect = { chosen ->
                sheet = null
                controller.openSession(chosen)
            },
        )
        HomeSheet.Gallery -> GallerySheet(controller, onPickImage = ::chooseImage) { sheet = null }
        HomeSheet.Organize -> {
            val target = controller.snapshot?.sessions?.firstOrNull { it.id == organizeID } ?: session
            if (target != null) {
                OrganizeSheet(controller, target, onDismiss = { sheet = null }, onRemove = { confirmingRemove = true })
            }
        }
        HomeSheet.Presets -> presetProject?.let { project ->
            PresetSheet(controller, project) { sheet = null }
        }
        null -> Unit
    }
    if (confirmingRemove) {
        AlertDialog(
            onDismissRequest = { confirmingRemove = false },
            confirmButton = {
                TextButton(onClick = {
                    confirmingRemove = false
                    sheet = null
                    val id = organizeID ?: controller.openSessionID
                    if (id != null) controller.sessionAction(id, "remove")
                }) { Text("移除", color = Attention) }
            },
            dismissButton = { TextButton(onClick = { confirmingRemove = false }) { Text("取消") } },
            title = { Text("移除这个会话") },
            text = { Text("正在运行的进程会先停掉。会话从列表里消失，对话本身还在。") },
            containerColor = Sheet,
        )
    }
    controller.artifactImage?.let { bitmap ->
        AlertDialog(
            onDismissRequest = controller::dismissArtifact,
            confirmButton = { TextButton(onClick = controller::dismissArtifact) { Text("关闭") } },
            text = { Image(bitmap = bitmap.asImageBitmap(), contentDescription = null) },
            containerColor = Sheet,
        )
    }
    annotate?.let { bitmap ->
        ImageAnnotator(
            source = bitmap,
            onCancel = { annotate = null },
            onDone = { painted ->
                annotate = null
                val maxBytes = if (controller.usingRelay) 250_000 else 3_500_000
                controller.attachImage(compressJpeg(painted, maxBytes))
                if (painted !== bitmap) painted.recycle()
            },
        )
    }
    }
}

@Composable
private fun EmptyTerminal(disconnected: Boolean, hostName: String?, onOpen: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Box(Modifier.size(76.dp).clip(RoundedCornerShape(22.dp)).background(Color(0xFF2A2118)), contentAlignment = Alignment.Center) {
            Text("U", color = Color(0xFFF2A05A), fontSize = 36.sp, fontWeight = FontWeight.SemiBold)
        }
        Spacer(Modifier.height(18.dp))
        Text(
            if (disconnected) "正在连接 ${hostName ?: "Host"}…" else "打开会话",
            color = Foreground.copy(alpha = 0.62f),
            fontSize = 15.sp,
        )
        if (!disconnected) {
            Spacer(Modifier.height(16.dp))
            TextButton(onClick = onOpen) {
                Text("会话列表", color = Foreground, fontWeight = FontWeight.SemiBold)
            }
        }
    }
}

@Composable
private fun TerminalSurface(
    controller: Controller,
    session: SessionSummary,
    onSessions: () -> Unit,
    onPickImage: () -> Unit,
    onSheet: (HomeSheet) -> Unit,
) {
    val project = controller.snapshot?.projects?.firstOrNull { it.id == session.projectID }
    val context = LocalContext.current
    val clipboard = LocalClipboardManager.current
    val voice = remember { VoiceCapture(context) }
    var listening by remember { mutableStateOf(false) }
    var voiceText by remember { mutableStateOf("") }
    DisposableEffect(voice) { onDispose { voice.cancel() } }
    DisposableEffect(session.id) { onDispose { controller.releasePhoneFit() } }
    val micPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) listening = true else controller.reportError("需要麦克风权限才能听写")
    }
    LaunchedEffect(listening) {
        if (!listening) return@LaunchedEffect
        voiceText = ""
        voice.start(
            onPartial = { voiceText = it },
            onFinal = { text ->
                listening = false
                voiceText = ""
                if (text.isNotEmpty()) controller.sendRaw(TerminalInput.paste(text))
            },
            onError = { message ->
                listening = false
                voiceText = ""
                controller.reportError(message)
            },
        )
    }
    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            GlassButton(onClick = onSessions) { Text("≡", color = Foreground, fontSize = 18.sp) }
            Column(
                Modifier.weight(1f).padding(horizontal = 8.dp).combinedClickable(onClick = {}, onLongClick = { onSheet(HomeSheet.Organize) }),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(session.title.ifEmpty { session.command }, color = Color.White, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                val context = listOfNotNull(project?.name, session.worktreeBranch ?: project?.gitBranch).joinToString("  ")
                if (context.isNotEmpty()) {
                    Text(context, color = Color.White.copy(alpha = 0.55f), fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
            if (session.providerID != null || session.activeRuntimeID != null) {
                GlassButton(onClick = {
                    controller.loadArtifacts()
                    onSheet(HomeSheet.Gallery)
                }) { Text("▣", color = Foreground, fontSize = 16.sp) }
                Spacer(Modifier.width(8.dp))
            }
            val spinning = controller.snapshot?.sessions?.any { it.activity == "working" || it.activity == "starting" } == true
            val blocked = controller.snapshot?.sessions?.any { it.activity == "blocked" } == true
            GlassButton(onClick = { onSheet(HomeSheet.Activity) }) {
                if (spinning) BrailleSpinner(if (blocked) Attention else Foreground) else Text("🔔", fontSize = 14.sp)
            }
        }
        TerminalGrid(controller)
        if (listening) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    voiceText.ifEmpty { "正在听…" },
                    color = Foreground,
                    fontSize = 13.sp,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = {
                    listening = false
                    voice.cancel()
                    voiceText = ""
                }) { Text("取消") }
                TextButton(onClick = { voice.stop() }) { Text("完成") }
            }
        }
        controller.error?.let { message ->
            Row(Modifier.fillMaxWidth().padding(horizontal = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(message, color = Color(0xFFE85D5D), fontSize = 12.sp, modifier = Modifier.weight(1f), maxLines = 2)
                TextButton(onClick = controller::dismissError) { Text("关闭") }
            }
        }
        Row(
            Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 6.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            KeyChip("粘贴") {
                val text = clipboard.getText()?.text.orEmpty()
                if (text.isNotEmpty()) controller.sendRaw(TerminalInput.pasteAndSubmit(text))
            }
            KeyChip(if (listening) "停" else "语音") {
                if (listening) {
                    voice.stop()
                } else if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
                    listening = true
                } else {
                    micPermission.launch(Manifest.permission.RECORD_AUDIO)
                }
            }
            KeyChip("图片") { onPickImage() }
            KeyChip("←") { controller.sendKey("arrowLeft") }
            KeyChip("↑") { controller.sendKey("arrowUp") }
            KeyChip("↓") { controller.sendKey("arrowDown") }
            KeyChip("→") { controller.sendKey("arrowRight") }
            KeyChip("Esc") { controller.sendKey("escape") }
            KeyChip("Tab") { controller.sendKey("tab") }
            KeyChip("^C") { controller.sendKey("controlC") }
            KeyChip("/") { controller.composer += "/" }
            KeyChip("@") { controller.composer += "@" }
        }
        Row(
            Modifier
                .fillMaxWidth()
                .windowInsetsPadding(WindowInsets.ime.union(WindowInsets.navigationBars))
                .padding(start = 20.dp, end = 20.dp, bottom = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedTextField(
                value = controller.composer,
                onValueChange = { controller.composer = it },
                modifier = Modifier.weight(1f),
                placeholder = { Text("发给这个会话", color = Muted) },
                singleLine = true,
                shape = RoundedCornerShape(24.dp),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedTextColor = Foreground,
                    unfocusedTextColor = Foreground,
                    cursorColor = Foreground,
                    focusedBorderColor = Color.White.copy(alpha = 0.35f),
                    unfocusedBorderColor = Color.White.copy(alpha = 0.16f),
                    focusedContainerColor = Color.White.copy(alpha = 0.06f),
                    unfocusedContainerColor = Color.White.copy(alpha = 0.06f),
                ),
            )
            Text(
                "发送",
                color = Foreground,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clip(RoundedCornerShape(24.dp))
                    .background(Color.White.copy(alpha = 0.10f))
                    .clickable(onClick = controller::sendComposer)
                    .padding(horizontal = 16.dp, vertical = 14.dp),
            )
        }
    }
}

private data class CellSel(val anchorRow: Int, val anchorCol: Int, val focusRow: Int, val focusCol: Int)

private fun CellSel.ordered(): CellSel {
    val forward = anchorRow < focusRow || (anchorRow == focusRow && anchorCol <= focusCol)
    return if (forward) this else CellSel(focusRow, focusCol, anchorRow, anchorCol)
}

private fun CellSel.contains(row: Int, col: Int): Boolean {
    val sel = ordered()
    if (row < sel.anchorRow || row > sel.focusRow) return false
    if (sel.anchorRow == sel.focusRow) return col in sel.anchorCol..sel.focusCol
    if (row == sel.anchorRow) return col >= sel.anchorCol
    if (row == sel.focusRow) return col <= sel.focusCol
    return true
}

@Composable
private fun ColumnScope.TerminalGrid(controller: Controller) {
    val revision = controller.screenRevision
    val history = remember(revision) { controller.screen.scrollback() }
    val view = remember(revision) { controller.screen.snapshot() }
    val cursor = remember(revision) { controller.screen.cursor() }
    val all = history + view
    val tailStart = (all.size - 300).coerceAtLeast(0)
    val tail = if (tailStart == 0) all else all.subList(tailStart, all.size)
    val cursorRow = history.size + cursor.first - tailStart
    val cursorCol = cursor.second
    val scroll = rememberScrollState()
    var stick by remember { mutableStateOf(true) }
    LaunchedEffect(scroll.isScrollInProgress) {
        if (!scroll.isScrollInProgress) {
            stick = scroll.maxValue == 0 || scroll.value >= scroll.maxValue - 48
        }
    }
    LaunchedEffect(revision, scroll.maxValue) {
        if (stick) scroll.scrollTo(scroll.maxValue)
    }
    val style = TextStyle(
        fontFamily = FontFamily.Monospace,
        fontSize = 13.sp,
        lineHeight = 16.sp,
        platformStyle = PlatformTextStyle(includeFontPadding = false),
    )
    val density = LocalDensity.current
    val cellW = rememberTextMeasurer().measure(AnnotatedString("M"), style = style).size.width.toFloat().coerceAtLeast(1f)
    val cellH = with(density) { 16.sp.toPx() }.coerceAtLeast(1f)
    val insetPx = with(density) { 8.dp.toPx() }
    val imeBottom = WindowInsets.ime.getBottom(density)
    var phoneFitSent by remember(controller.openSessionID) { mutableStateOf(false) }
    var gridSize by remember(controller.openSessionID) { mutableStateOf(IntSize.Zero) }
    LaunchedEffect(controller.openSessionID, gridSize, imeBottom, controller.connection) {
        if (phoneFitSent || imeBottom > 0) return@LaunchedEffect
        val grid = phoneFitGrid(gridSize.width, gridSize.height, cellW, cellH, insetPx) ?: return@LaunchedEffect
        delay(120)
        if (phoneFitSent || imeBottom > 0) return@LaunchedEffect
        if (controller.fitPhone(grid.first, grid.second)) phoneFitSent = true
    }
    var selection by remember { mutableStateOf<CellSel?>(null) }
    val clipboard = LocalClipboardManager.current
    val cols = tail.firstOrNull()?.size ?: 1
    Box(
        Modifier
            .weight(1f)
            .padding(horizontal = 8.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(Canvas)
            .onSizeChanged { gridSize = it },
    ) {
        Column(
            Modifier
                .fillMaxSize()
                .verticalScroll(scroll)
                .horizontalScroll(rememberScrollState())
                .padding(8.dp)
                .pointerInput(tail.size, cols, cellW, cellH) {
                    detectDragGesturesAfterLongPress(
                        onDragStart = { offset ->
                            val hit = hitCell(offset.x, offset.y, cellW, cellH, tail.size, cols)
                            selection = CellSel(hit.first, hit.second, hit.first, hit.second)
                        },
                        onDrag = { change, _ ->
                            val hit = hitCell(change.position.x, change.position.y, cellW, cellH, tail.size, cols)
                            selection = selection?.copy(focusRow = hit.first, focusCol = hit.second)
                        },
                    )
                },
        ) {
            tail.forEachIndexed { index, row ->
                Text(terminalLine(row, index, cursorRow, cursorCol, selection), style = style, maxLines = 1, softWrap = false)
            }
        }
        selection?.let { sel ->
            Row(Modifier.align(Alignment.TopEnd).padding(4.dp)) {
                TextButton(onClick = {
                    clipboard.setText(AnnotatedString(selectedText(tail, sel)))
                    selection = null
                }) { Text("复制") }
                TextButton(onClick = { selection = null }) { Text("取消") }
            }
        }
    }
}

/** Columns and rows that letterbox the Host PTY to this terminal box. Null until the box is real. */
internal fun phoneFitGrid(widthPx: Int, heightPx: Int, cellW: Float, cellH: Float, insetPx: Float): Pair<Int, Int>? {
    val innerW = widthPx - insetPx * 2
    val innerH = heightPx - insetPx * 2
    if (innerW < 48f || innerH < 48f || cellW <= 0f || cellH <= 0f) return null
    val columns = (innerW / cellW).toInt().coerceIn(2, 300)
    val rows = (innerH / cellH).toInt().coerceIn(2, 120)
    return columns to rows
}

private fun hitCell(x: Float, y: Float, cellW: Float, cellH: Float, rows: Int, cols: Int): Pair<Int, Int> {
    val row = (y / cellH).toInt().coerceIn(0, (rows - 1).coerceAtLeast(0))
    val col = (x / cellW).toInt().coerceIn(0, (cols - 1).coerceAtLeast(0))
    return row to col
}

private fun terminalLine(
    row: List<VtScreen.Cell>,
    rowIndex: Int,
    cursorRow: Int,
    cursorCol: Int,
    selection: CellSel?,
) = buildAnnotatedString {
    row.forEachIndexed { col, cell ->
        if (cell.ch == '\u0000') return@forEachIndexed
        val selected = selection?.contains(rowIndex, col) == true
        val atCursor = selection == null && rowIndex == cursorRow && col == cursorCol
        val fg = when {
            selected -> Color(0xFF121314)
            atCursor -> Canvas
            else -> rgb(cell.fg)
        }
        val bg = when {
            selected -> Color(0xFFD0D0D0)
            atCursor -> rgb(cell.fg)
            else -> rgb(cell.bg)
        }
        withStyle(SpanStyle(color = fg, background = bg, fontWeight = if (cell.bold) FontWeight.Bold else null)) {
            append(cell.ch)
        }
    }
}

private fun selectedText(rows: List<List<VtScreen.Cell>>, selection: CellSel): String {
    if (rows.isEmpty()) return ""
    val sel = selection.ordered()
    return (sel.anchorRow..sel.focusRow).joinToString("\n") { rowIndex ->
        val row = rows.getOrNull(rowIndex).orEmpty()
        val from = if (rowIndex == sel.anchorRow) sel.anchorCol else 0
        val to = if (rowIndex == sel.focusRow) sel.focusCol else row.lastIndex
        row.filterIndexed { col, cell -> col in from..to && cell.ch != '\u0000' }
            .joinToString("") { it.ch.toString() }
            .trimEnd()
    }
}

@Composable
private fun SessionDrawer(
    controller: Controller,
    selectedID: String?,
    width: Dp,
    onDrag: (Float) -> Unit,
    onRelease: () -> Unit,
    onClose: () -> Unit,
    onWorkspaces: () -> Unit,
    onNewSession: (ProjectSummary) -> Unit,
    onOrganize: (SessionSummary) -> Unit,
) {
    val snapshot = controller.snapshot
    var expanded by remember { mutableStateOf(setOf<String>()) }
    LaunchedEffect(selectedID) {
        val projectID = snapshot?.sessions?.firstOrNull { it.id == selectedID }?.projectID
        if (!projectID.isNullOrEmpty()) expanded = expanded + projectID
    }
    Column(
        Modifier
            .fillMaxHeight()
            .width(width)
            .clip(RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp))
            .background(Frame)
            .pointerInput(Unit) {
                detectHorizontalDragGestures(
                    onHorizontalDrag = { _, amount -> onDrag(amount) },
                    onDragEnd = { onRelease() },
                    onDragCancel = { onRelease() },
                )
            },
    ) {
        WorkspaceHeader(controller, onWorkspaces)
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 8.dp, vertical = 8.dp)) {
            if (snapshot == null) {
                Text(controller.connection, color = Muted, fontSize = 13.sp, modifier = Modifier.padding(16.dp))
            } else {
                val projects = snapshot.projects.filter { it.parentProjectID == null }
                projects.forEach { project ->
                    ProjectBlock(project, snapshot.projects, snapshot.sessions, expanded, selectedID, onToggle = {
                        expanded = if (project.id in expanded) expanded - project.id else expanded + project.id
                    }, onNewSession = onNewSession, onOpen = {
                        controller.openSession(it.id)
                        onClose()
                    }, onOrganize = onOrganize)
                }
            }
        }
        val context = LocalContext.current
        Text(
            "反馈与问题",
            color = Muted,
            fontSize = 14.sp,
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .border(width = 1.dp, color = Hairline)
                .clickable {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://github.com/orgs/unpeel-com/discussions")))
                }
                .padding(horizontal = 16.dp, vertical = 16.dp),
        )
    }
}

@Composable
private fun WorkspaceHeader(controller: Controller, onClick: () -> Unit) {
    val title = controller.selectedHost?.macName ?: controller.snapshot?.macName ?: "Host"
    val failing = controller.snapshot == null && controller.connection != "正在连接" && controller.connection != "未配对"
    val hostCount = controller.hosts.size
    val link = when {
        controller.snapshot == null -> if (failing) "连接中断" else "正在连接…"
        controller.usingRelay -> "经由 Link"
        else -> null
    }
    val subtitle = buildString {
        append(if (hostCount > 1) "$hostCount 台 Host · 点按切换" else "点按切换 Host")
        if (link != null) append(" · ").append(link)
    }
    Row(
        Modifier
            .padding(start = 8.dp, end = 8.dp, top = 12.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Hover)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier.size(10.dp).clip(CircleShape).background(
                when {
                    failing -> Attention
                    controller.snapshot == null -> Muted.copy(alpha = 0.5f)
                    else -> Color(0xFF3DDC84)
                },
            ),
        )
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(title, color = Foreground, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(subtitle, color = Muted, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Text(
            "切换",
            color = Foreground,
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier
                .clip(RoundedCornerShape(12.dp))
                .background(Color.White.copy(alpha = 0.10f))
                .padding(horizontal = 12.dp, vertical = 8.dp),
        )
    }
}

@Composable
private fun ProjectBlock(
    project: ProjectSummary,
    projects: List<ProjectSummary>,
    sessions: List<SessionSummary>,
    expanded: Set<String>,
    selectedID: String?,
    onToggle: () -> Unit,
    onNewSession: (ProjectSummary) -> Unit,
    onOpen: (SessionSummary) -> Unit,
    onOrganize: (SessionSummary) -> Unit,
) {
    val open = project.id in expanded
    val own = sessions.filter { it.projectID == project.id }
    val attention = own.any { it.activity == "blocked" }
    Row(Modifier.fillMaxWidth().height(48.dp).clickable(onClick = onToggle), verticalAlignment = Alignment.CenterVertically) {
        Text(if (open) "▾" else "▸", color = Muted, fontSize = 16.sp, modifier = Modifier.width(22.dp))
        Text(project.name, color = if (project.isGroup) Foreground else Foreground.copy(alpha = 0.62f), fontSize = 16.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
        if (attention) Box(Modifier.padding(end = 8.dp).size(8.dp).clip(CircleShape).background(Attention))
        val branch = project.worktreeBranch ?: project.gitBranch
        if (!branch.isNullOrEmpty() && branch != project.name) {
            Text(branch, color = Muted.copy(alpha = 0.55f), fontSize = 12.sp, fontFamily = FontFamily.Monospace, maxLines = 1, modifier = Modifier.widthIn(max = 96.dp).padding(end = 4.dp))
        }
        if (!project.isGroup) {
            Box(
                Modifier.size(36.dp).clip(CircleShape).clickable { onNewSession(project) },
                contentAlignment = Alignment.Center,
            ) {
                Text("+", color = Foreground, fontSize = 22.sp)
            }
        }
    }
    if (open) {
        projects.filter { it.parentProjectID == project.id }.forEach { child ->
            Text(child.name, color = Muted, fontSize = 15.sp, modifier = Modifier.padding(start = 32.dp, top = 8.dp, bottom = 8.dp))
        }
        val pinned = own.filter { it.pinned }
        val rest = own.filter { !it.pinned }
        (pinned + rest).forEach { session ->
            SessionLine(session, session.id == selectedID, onOpen = { onOpen(session) }, onOrganize = { onOrganize(session) })
        }
    }
}

@Composable
private fun SessionLine(session: SessionSummary, selected: Boolean, onOpen: () -> Unit, onOrganize: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(start = 8.dp, top = 4.dp, bottom = 4.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(if (selected) ActiveRow else Color.Transparent)
            .combinedClickable(onClick = onOpen, onLongClick = onOrganize)
            .padding(horizontal = 10.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
            when {
                session.status == "running" && session.activity == "blocked" ->
                    Box(Modifier.size(8.dp).clip(CircleShape).background(Attention))
                session.status == "running" && (session.activity == "working" || session.activity == "starting") ->
                    BrailleSpinner(rgb(session.spinnerColorHex ?: 0xB9BDC9))
                session.unread -> Box(Modifier.size(8.dp).clip(CircleShape).background(Unread))
            }
        }
        Spacer(Modifier.width(6.dp))
        BrandMark(session.command, size = 28.dp)
        Text(
            session.title.ifEmpty { session.command },
            color = if (session.status == "exited") Muted else Foreground,
            fontSize = 16.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f).padding(start = 10.dp, end = 8.dp),
        )
        if (session.pinned) Text("📌", fontSize = 14.sp, modifier = Modifier.padding(end = 8.dp))
        Text(
            if (session.activity == "blocked") "blocked" else relativeAge(session.updatedAtUnixMs),
            color = Muted,
            fontSize = 12.sp,
        )
    }
}

@Composable
private fun ApprovalOverlay(approval: PendingApproval, more: Int, onAnswer: (String, Boolean) -> Unit) {
    Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.28f)), contentAlignment = Alignment.Center) {
        Column(
            Modifier.widthIn(max = 340.dp).padding(horizontal = 28.dp).clip(RoundedCornerShape(22.dp)).background(Sheet).border(1.dp, Hairline, RoundedCornerShape(22.dp)).padding(22.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("?", color = Color.Yellow, fontSize = 28.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(10.dp))
            Text(approval.title, color = Foreground, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            Text(approval.body, color = Muted, fontSize = 13.sp)
            if (more > 0) {
                Spacer(Modifier.height(6.dp))
                Text("还有 $more 个在等", color = Muted.copy(alpha = 0.7f), fontSize = 11.sp)
            }
            Spacer(Modifier.height(14.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                TextButton(onClick = { onAnswer(approval.id, false) }, modifier = Modifier.weight(1f)) { Text("不允许") }
                Button(onClick = { onAnswer(approval.id, true) }, modifier = Modifier.weight(1f)) { Text("允许") }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HostSheet(controller: Controller, onDismiss: () -> Unit) {
    val context = LocalContext.current
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 18.dp, vertical = 8.dp).verticalScroll(rememberScrollState())) {
            Text("Host", color = Foreground, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            Text("一台手机可以记住多台 Mac。点一台切换，下面可以再配一台。", color = Muted, fontSize = 13.sp)
            Spacer(Modifier.height(14.dp))
            controller.hosts.forEach { host ->
                val selected = host.macID == controller.selectedHost?.macID
                Row(
                    Modifier
                        .fillMaxWidth()
                        .padding(vertical = 6.dp)
                        .clip(RoundedCornerShape(16.dp))
                        .background(if (selected) ActiveRow else Hover)
                        .padding(horizontal = 14.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f).clickable {
                        controller.select(host.macID)
                        onDismiss()
                    }) {
                        Text(host.macName, color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text(
                            if (selected) "当前 · ${host.endpoint}" else host.endpoint,
                            color = Muted,
                            fontSize = 13.sp,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    TextButton(onClick = { controller.forget(host.macID) }) { Text("忘记", color = Attention, fontSize = 15.sp) }
                }
            }
            val workspaces = controller.snapshot?.workspaces.orEmpty()
            if (workspaces.size > 1) {
                Spacer(Modifier.height(16.dp))
                Text("这台 Host 上的工作区", color = Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(6.dp))
                workspaces.forEach { workspace ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .padding(vertical = 4.dp)
                            .clip(RoundedCornerShape(14.dp))
                            .background(if (workspace.isCurrent) ActiveRow else Color.Transparent)
                            .clickable {
                                controller.selectWorkspace(workspace.id)
                                onDismiss()
                            }
                            .padding(horizontal = 14.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(workspace.name, color = Foreground, fontSize = 16.sp, modifier = Modifier.weight(1f))
                        if (workspace.isCurrent) Text("当前", color = Muted, fontSize = 13.sp)
                    }
                }
            }
            Spacer(Modifier.height(16.dp))
            Text("添加另一台", color = Foreground, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            PairFields(controller, context)
            Spacer(Modifier.height(24.dp))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ActivitySheet(controller: Controller, onDismiss: () -> Unit, onSelect: (String) -> Unit) {
    val sessions = controller.snapshot?.sessions.orEmpty()
    val projects = controller.snapshot?.projects.orEmpty().associateBy { it.id }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = Color(0xFF16171C)) {
        Column(Modifier.padding(horizontal = 12.dp).verticalScroll(rememberScrollState())) {
            ActivityGroup("等待处理", sessions.filter { it.activity == "blocked" }, projects, onSelect)
            ActivityGroup("正在运行", sessions.filter { it.activity == "working" || it.activity == "starting" }, projects, onSelect)
            ActivityGroup("最近", sessions.filter { it.activity != "blocked" && it.activity != "working" && it.activity != "starting" }.take(12), projects, onSelect)
            if (sessions.isEmpty()) Text("没有正在进行的会话", color = Muted, modifier = Modifier.padding(18.dp), fontSize = 13.sp)
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun ActivityGroup(title: String, sessions: List<SessionSummary>, projects: Map<String, ProjectSummary>, onSelect: (String) -> Unit) {
    if (sessions.isEmpty()) return
    Text(title, color = Color.White.copy(alpha = 0.46f), fontSize = 11.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(start = 8.dp, top = 12.dp, bottom = 4.dp))
    sessions.forEach { session ->
        Row(Modifier.fillMaxWidth().clickable { onSelect(session.id) }.padding(horizontal = 8.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
                when {
                    session.activity == "working" || session.activity == "starting" -> BrailleSpinner(rgb(session.spinnerColorHex ?: 0xB9BDC9))
                    session.activity == "blocked" -> Box(Modifier.size(7.dp).clip(CircleShape).background(Attention))
                    session.unread -> Box(Modifier.size(7.dp).clip(CircleShape).background(Unread))
                    else -> BrandMark(session.command, size = 18.dp)
                }
            }
            Column(Modifier.weight(1f).padding(start = 8.dp)) {
                Text(session.title.ifEmpty { session.command }, color = Color.White.copy(alpha = 0.9f), fontSize = 14.sp, maxLines = 1)
                Text(projects[session.projectID]?.name ?: session.command, color = Color.White.copy(alpha = 0.48f), fontSize = 11.sp, maxLines = 1)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GallerySheet(controller: Controller, onPickImage: () -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 16.dp).verticalScroll(rememberScrollState())) {
            Text("截图和图库", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            TextButton(onClick = controller::requestScreenshot) { Text("现在截一张", color = Foreground) }
            TextButton(onClick = {
                onDismiss()
                onPickImage()
            }) { Text("从相册标注并附上", color = Foreground) }
            if (controller.artifacts.isEmpty()) Text("还没有截图。", color = Muted, fontSize = 13.sp)
            controller.artifacts.forEach { artifact ->
                TextButton(onClick = { controller.openArtifact(artifact.kind, artifact.name) }) {
                    Text("${artifact.kind}  ${artifact.name}", color = Foreground)
                }
            }
            Spacer(Modifier.height(20.dp))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun OrganizeSheet(controller: Controller, session: SessionSummary, onDismiss: () -> Unit, onRemove: () -> Unit) {
    var title by remember(session.id) { mutableStateOf(session.title) }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 18.dp)) {
            Text("编辑会话", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(10.dp))
            OutlinedTextField(value = title, onValueChange = { title = it }, singleLine = true, modifier = Modifier.fillMaxWidth())
            Row {
                TextButton(onClick = {
                    controller.organize(session.id, title = title.trim())
                    onDismiss()
                }, enabled = title.isNotBlank()) { Text("保存名称") }
                TextButton(onClick = { controller.organize(session.id, pinned = !session.pinned) }) {
                    Text(if (session.pinned) "取消置顶" else "置顶")
                }
            }
            TextButton(onClick = { controller.sessionAction(session.id, "stop") }) { Text("停止") }
            TextButton(onClick = { controller.sessionAction(session.id, "restart") }) { Text("重启") }
            TextButton(onClick = onRemove) { Text("移除", color = Attention) }
            Spacer(Modifier.height(18.dp))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PresetSheet(controller: Controller, project: ProjectSummary, onDismiss: () -> Unit) {
    val presets = controller.snapshot?.presets?.filter { it.enabled }.orEmpty()
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 12.dp)) {
            Text("新建会话", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(horizontal = 8.dp))
            Text(project.name, color = Muted, fontSize = 12.sp, modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp))
            presets.forEach { preset ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .padding(vertical = 4.dp)
                        .clip(RoundedCornerShape(14.dp))
                        .background(Hover)
                        .clickable {
                            controller.createSession(project.id, preset.id, "")
                            onDismiss()
                        }
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    BrandMark(preset.command)
                    Column(Modifier.padding(start = 10.dp)) {
                        Text(preset.label.ifEmpty { preset.command }, color = Foreground, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
                        Text(preset.command, color = Muted, fontSize = 11.sp, fontFamily = FontFamily.Monospace, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
            if (presets.isEmpty()) Text("这台 Host 没有可用的预设。", color = Muted, modifier = Modifier.padding(12.dp))
            Spacer(Modifier.height(20.dp))
        }
    }
}

@Composable
private fun PairScreen(controller: Controller, firstRun: Boolean) {
    val context = LocalContext.current
    Column(
        Modifier.fillMaxSize().background(Frame).navigationBarsPadding().verticalScroll(rememberScrollState()).padding(20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Spacer(Modifier.height(28.dp))
        Box(Modifier.size(68.dp).clip(RoundedCornerShape(20.dp)).background(Color(0xFF2A2118)), contentAlignment = Alignment.Center) {
            Text("U", color = Color(0xFFF2A05A), fontSize = 32.sp, fontWeight = FontWeight.SemiBold)
        }
        Spacer(Modifier.height(12.dp))
        Text(if (firstRun) "配对一台工作区" else "Unpeel", color = Color.White, fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(16.dp))
        Text(
            "在 Mac 上打开 Unpeel 的设置，找到手机配对，扫那里的二维码。这台手机只控制 Host，不在本地开终端。",
            color = Foreground.copy(alpha = 0.72f),
            fontSize = 14.sp,
            modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(Color.White.copy(alpha = 0.06f)).padding(14.dp),
        )
        Spacer(Modifier.height(16.dp))
        PairFields(controller, context)
    }
}

@Composable
private fun PairFields(controller: Controller, context: android.content.Context) {
    var code by remember { mutableStateOf("") }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) controller.reportError("需要相机权限才能扫配对码")
    }
    val scan = rememberLauncherForActivityResult(ScanContract()) { result ->
        result.contents?.let { controller.pair(it) }
    }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Button(
            onClick = {
                val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
                if (granted) scan.launch(ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE).setPrompt("对准 Unpeel 配对码"))
                else permission.launch(Manifest.permission.CAMERA)
            },
            modifier = Modifier.fillMaxWidth(),
        ) { Text("扫描二维码") }
        OutlinedTextField(value = code, onValueChange = { code = it }, modifier = Modifier.fillMaxWidth(), label = { Text("或粘贴配对码") }, minLines = 2)
        TextButton(onClick = { controller.pair(code) }, enabled = !controller.busy && code.isNotBlank()) { Text("用这段配对") }
        controller.error?.let { Text(it, color = Color(0xFFE85D5D), fontSize = 13.sp) }
    }
}

@Composable
private fun GlassButton(onClick: () -> Unit, content: @Composable () -> Unit) {
    val fill = Brush.verticalGradient(listOf(Color.White.copy(alpha = 0.28f), Color.White.copy(alpha = 0.08f)))
    val rim = Brush.verticalGradient(listOf(Color.White.copy(alpha = 0.55f), Color.White.copy(alpha = 0.08f)))
    Box(
        Modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(fill)
            .border(1.dp, rim, CircleShape)
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) { content() }
}

@Composable
private fun KeyChip(label: String, onClick: () -> Unit) {
    Text(
        label,
        color = Color.White.copy(alpha = 0.92f),
        fontSize = 13.sp,
        modifier = Modifier.clip(RoundedCornerShape(10.dp)).background(KeyFill).clickable(onClick = onClick).padding(horizontal = 12.dp, vertical = 8.dp),
    )
}

@Composable
private fun BrailleSpinner(color: Color) {
    var frame by remember { mutableIntStateOf(0) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(120)
            frame = (frame + 1) % SpinnerFrames.size
        }
    }
    Text(SpinnerFrames[frame], color = color, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold, fontSize = 14.sp)
}

private fun rgb(hex: Int): Color {
    val argb = if (hex ushr 24 == 0) hex or 0xFF000000.toInt() else hex
    return Color(argb)
}

private fun relativeAge(unixMs: Long?): String {
    if (unixMs == null || unixMs <= 0L) return ""
    val age = ((System.currentTimeMillis() - unixMs).coerceAtLeast(0L)) / 1000
    return when {
        age < 60 -> "now"
        age < 3600 -> "${age / 60}m"
        age < 86_400 -> "${age / 3600}h"
        else -> "${age / 86_400}d"
    }
}
