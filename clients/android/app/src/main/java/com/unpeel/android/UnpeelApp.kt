@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class, androidx.compose.material3.ExperimentalMaterial3Api::class)

package com.unpeel.android

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.material3.Switch
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
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import com.unpeel.controller.AgentCatalog
import com.unpeel.controller.PendingApproval
import com.unpeel.controller.ProjectSummary
import com.unpeel.controller.ResumeOffer
import com.unpeel.controller.SessionSummary
import com.unpeel.controller.SidebarEntry
import com.unpeel.controller.TerminalInput
import com.unpeel.controller.canNotifyWhenDone
import com.unpeel.controller.canStopSession
import com.unpeel.controller.folderColorArgb
import com.unpeel.controller.resumeOffer
import com.unpeel.controller.sidebarEntries
import com.unpeel.controller.sidebarIndex
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
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

private enum class HomeSheet { Workspaces, Activity, Gallery, Organize, Presets, Project }

@Composable
fun UnpeelApp(controller: Controller) {
    val context = LocalContext.current
    val activity = context as? Activity
    val lock = remember { AppLock(context) }
    val lifecycleOwner = LocalLifecycleOwner.current
    val credential = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        lock.finishCredential(result.resultCode == Activity.RESULT_OK)
    }
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) lock.onBackground()
            if (event == Lifecycle.Event.ON_START && activity != null) {
                lock.onForeground(activity)?.let { credential.launch(it) }
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    LaunchedEffect(activity) {
        if (activity != null) lock.onForeground(activity)?.let { credential.launch(it) }
    }
    val chrome = hostChrome(controller.snapshot?.hostTintHue)
    MaterialTheme(colorScheme = darkColorScheme(background = chrome, surface = Sheet, primary = Foreground)) {
        Box(Modifier.fillMaxSize().background(chrome).windowInsetsPadding(WindowInsets.statusBars)) {
            if (controller.selectedHost == null) {
                PairScreen(controller, firstRun = true)
            } else {
                TerminalHome(controller, locked = lock.locked, lock = lock, onCredential = { credential.launch(it) })
            }
            if (lock.locked) {
                AppLockCover(lock, onUnlock = {
                    if (activity != null) lock.unlock(activity)?.let { credential.launch(it) }
                })
            }
        }
    }
}

@Composable
private fun TerminalHome(
    controller: Controller,
    locked: Boolean,
    lock: AppLock,
    onCredential: (Intent) -> Unit,
) {
    var sheet by remember { mutableStateOf<HomeSheet?>(null) }
    var presetProject by remember { mutableStateOf<ProjectSummary?>(null) }
    var organizeID by remember { mutableStateOf<String?>(null) }
    var organizeProjectID by remember { mutableStateOf<String?>(null) }
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
    Box(Modifier.fillMaxSize().background(hostChrome(controller.snapshot?.hostTintHue))) {
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
                    onOrganizeProject = { project ->
                        organizeProjectID = project.id
                        sheet = HomeSheet.Project
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
    if (!locked) when (sheet) {
        HomeSheet.Workspaces -> HostSheet(controller, lock, onCredential) { sheet = null }
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
        HomeSheet.Project -> {
            val project = controller.snapshot?.projects?.firstOrNull { it.id == organizeProjectID }
            if (project != null) {
                ProjectOrganizeSheet(controller, project) { sheet = null }
            }
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
    val voice = remember { VoiceCapture(context) }
    var listening by remember { mutableStateOf(false) }
    var voiceText by remember { mutableStateOf("") }
    var ctrlArmed by remember { mutableStateOf(false) }
    var allowComposerFocus by remember { mutableStateOf(false) }
    val composerFocus = remember { FocusRequester() }
    val focusManager = LocalFocusManager.current
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_START || event == Lifecycle.Event.ON_RESUME || event == Lifecycle.Event.ON_STOP) {
                allowComposerFocus = false
                focusManager.clearFocus(force = true)
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
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
                val branch = session.worktreeBranch ?: project?.gitBranch
                if (!project?.name.isNullOrEmpty() || !branch.isNullOrEmpty()) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        if (!project?.name.isNullOrEmpty()) {
                            Text(project?.name.orEmpty(), color = Color.White.copy(alpha = 0.55f), fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 120.dp))
                        }
                        if (!branch.isNullOrEmpty()) {
                            if (!project?.name.isNullOrEmpty()) Spacer(Modifier.width(4.dp))
                            BranchMark(11.dp, Color.White.copy(alpha = 0.55f))
                            Spacer(Modifier.width(2.dp))
                            Text(branch, color = Color.White.copy(alpha = 0.55f), fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 96.dp))
                        }
                    }
                }
            }
            if (AgentCatalog.isAgent(session.providerID, session.command)) {
                val pulse by animateFloatAsState(if (controller.galleryPulse) 1.12f else 1f, label = "gallery")
                Box(Modifier.graphicsLayer { scaleX = pulse; scaleY = pulse }) {
                    GlassButton(onClick = {
                        controller.loadArtifacts()
                        onSheet(HomeSheet.Gallery)
                    }) {
                        Text("▣", color = if (controller.galleryPulse) Attention else Foreground, fontSize = 16.sp)
                    }
                }
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
        if (session.status == "exited" && session.capabilities?.restart == true) {
            ExitedRestartBar(
                restarting = controller.restartingSessionID == session.id,
                onRestart = { controller.sessionAction(session.id, "restart") },
            )
        }
        Row(
            Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier.weight(1f).horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RoundKey("◀") { controller.sendModifiedKey("arrowLeft", ctrlArmed); ctrlArmed = false }
                RoundKey("▲") { controller.sendModifiedKey("arrowUp", ctrlArmed); ctrlArmed = false }
                RoundKey("▼") { controller.sendModifiedKey("arrowDown", ctrlArmed); ctrlArmed = false }
                RoundKey("▶") { controller.sendModifiedKey("arrowRight", ctrlArmed); ctrlArmed = false }
                KeyDot()
                RoundKey("esc", textSize = 11) { controller.sendModifiedKey("escape", ctrlArmed); ctrlArmed = false }
                RoundKey("⇥") { controller.sendModifiedKey("tab", ctrlArmed); ctrlArmed = false }
                RoundKey("⌃", armed = ctrlArmed) { ctrlArmed = !ctrlArmed }
                KeyDot()
                RoundKey("/") {
                    if (ctrlArmed) {
                        controller.sendRaw("\u001f")
                        ctrlArmed = false
                    } else {
                        controller.composer += "/"
                    }
                }
                RoundKey("@") {
                    if (ctrlArmed) {
                        controller.sendRaw("\u0000")
                        ctrlArmed = false
                    } else {
                        controller.composer += "@"
                    }
                }
            }
            Spacer(Modifier.width(8.dp))
            RoundKey(if (listening) "停" else "麦", armed = listening, textSize = 12) {
                if (listening) {
                    voice.stop()
                } else if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
                    listening = true
                } else {
                    micPermission.launch(Manifest.permission.RECORD_AUDIO)
                }
            }
        }
        Row(
            Modifier
                .fillMaxWidth()
                .windowInsetsPadding(WindowInsets.ime.union(WindowInsets.navigationBars))
                .padding(start = 16.dp, end = 16.dp, bottom = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.Bottom,
        ) {
            OutlinedTextField(
                value = controller.composer,
                onValueChange = { next ->
                    val added = next.removePrefix(controller.composer)
                    if (ctrlArmed && added.length == 1 && added[0].isLetter()) {
                        val code = added[0].lowercaseChar().code - 'a'.code + 1
                        if (code in 1..26) {
                            controller.sendRaw(code.toChar().toString())
                            ctrlArmed = false
                            return@OutlinedTextField
                        }
                    }
                    controller.composer = next
                },
                modifier = Modifier
                    .weight(1f)
                    .pointerInput(Unit) {
                        awaitPointerEventScope {
                            while (true) {
                                val event = awaitPointerEvent(PointerEventPass.Initial)
                                val pressed = event.changes.any { change -> change.pressed && !change.previousPressed }
                                if (pressed) allowComposerFocus = true
                            }
                        }
                    }
                    .focusRequester(composerFocus)
                    .onFocusChanged { state ->
                        if (state.isFocused && !allowComposerFocus) focusManager.clearFocus(force = true)
                    },
                placeholder = { Text("发给这个会话", color = Muted) },
                minLines = 1,
                maxLines = 5,
                shape = RoundedCornerShape(22.dp),
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
            Box(
                Modifier
                    .size(36.dp)
                    .clip(CircleShape)
                    .background(Color.White.copy(alpha = 0.14f))
                    .clickable(onClick = controller::sendComposer),
                contentAlignment = Alignment.Center,
            ) {
                Text("↑", color = Foreground, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
            }
        }
    }
}

@Composable
private fun ColumnScope.TerminalGrid(controller: Controller) {
    val density = LocalDensity.current
    val imeBottom = WindowInsets.ime.getBottom(density)
    var gridSize by remember(controller.openSessionID) { mutableStateOf(IntSize.Zero) }
    var cellW by remember { mutableFloatStateOf(0f) }
    var cellH by remember { mutableFloatStateOf(0f) }
    var topPad by remember { mutableFloatStateOf(0f) }
    var selecting by remember(controller.openSessionID) { mutableStateOf(false) }
    var surface by remember { mutableStateOf<HostTerminalView?>(null) }
    val clipboard = LocalClipboardManager.current
    LaunchedEffect(controller.openSessionID, gridSize, imeBottom, cellW, cellH, topPad, controller.connection) {
        if (gridSize.width < 48 || gridSize.height < 48 || cellW <= 0f || cellH <= 0f) return@LaunchedEffect
        delay(120)
        if (imeBottom > 0) {
            controller.notePhoneViewport(0, 0, keyboardOpen = true)
            return@LaunchedEffect
        }
        val grid = phoneFitGrid(gridSize.width, gridSize.height, cellW, cellH, topPad) ?: return@LaunchedEffect
        controller.notePhoneViewport(grid.first, grid.second, keyboardOpen = false)
    }
    Box(
        Modifier
            .weight(1f)
            .padding(horizontal = 8.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(Canvas),
    ) {
        AndroidView(
            modifier = Modifier.fillMaxSize().onSizeChanged { gridSize = it },
            factory = { context ->
                HostTerminalView(context, controller.terminal).also { view ->
                    surface = view
                    cellW = view.cellWidth
                    cellH = view.cellHeight.toFloat()
                    topPad = view.topPad.toFloat()
                    view.onSelectionChanged = { selecting = it }
                }
            },
            update = { view -> view.bind(controller.terminal) },
        )
        if (selecting) {
            Row(Modifier.align(Alignment.TopEnd).padding(4.dp)) {
                TextButton(onClick = {
                    val text = surface?.selectedText().orEmpty()
                    if (text.isNotEmpty()) clipboard.setText(AnnotatedString(text))
                    surface?.clearSelection()
                }) { Text("复制") }
                TextButton(onClick = { surface?.clearSelection() }) { Text("取消") }
            }
        }
    }
}

/** Columns and rows that letterbox the Host PTY to this terminal box. Null until the box is real. */
internal fun phoneFitGrid(widthPx: Int, heightPx: Int, cellW: Float, cellH: Float, topPadPx: Float): Pair<Int, Int>? {
    val innerH = heightPx - topPadPx
    if (widthPx < 48 || innerH < 48f || cellW <= 0f || cellH <= 0f) return null
    val columns = (widthPx / cellW).toInt().coerceIn(2, 300)
    val rows = (innerH / cellH).toInt().coerceIn(2, 120)
    return columns to rows
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
    onOrganizeProject: (ProjectSummary) -> Unit,
) {
    val snapshot = controller.snapshot
    var expanded by remember { mutableStateOf(setOf<String>()) }
    var openFolder by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(selectedID, snapshot?.projects, snapshot?.sessions) {
        val projectID = snapshot?.sessions?.firstOrNull { it.id == selectedID }?.projectID ?: return@LaunchedEffect
        val project = snapshot.projects.firstOrNull { it.id == projectID }
        val parent = snapshot.projects.firstOrNull { it.id == project?.parentProjectID }
        val folderID = project?.folderID ?: parent?.folderID
        if (folderID != null) openFolder = folderID
        expanded = expanded + listOfNotNull(projectID, project?.parentProjectID, parent?.parentProjectID)
    }
    Column(
        Modifier
            .fillMaxHeight()
            .width(width)
            .clip(RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp))
            .background(hostChrome(snapshot?.hostTintHue))
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
                val index = sidebarIndex(snapshot)
                val worktrees = snapshot.experimentalWorktreesEnabled
                index.folderGroups.forEach { group ->
                    val open = openFolder == group.folder.id
                    val memberSessions = group.projects.flatMap { project ->
                        val children = index.childrenByParent[project.id].orEmpty()
                            .filter { it.isGroup || (worktrees && it.worktreeBranch != null) }
                        index.sessionsByProject[project.id].orEmpty() +
                            children.flatMap { child -> index.sessionsByProject[child.id].orEmpty() }
                    }
                    FolderHeader(
                        name = group.folder.name,
                        colorID = group.folder.colorID,
                        open = open,
                        count = memberSessions.size,
                        attention = !open && memberSessions.any { it.activity == "blocked" },
                        onToggle = { openFolder = if (open) null else group.folder.id },
                    )
                    if (open) {
                        group.projects.forEach { project ->
                            ProjectBlock(
                                project, index, worktrees, expanded, selectedID,
                                onToggle = { id -> expanded = expanded.toggle(id) },
                                onNewSession = onNewSession,
                                onOpen = {
                                    controller.openSession(it.id)
                                    onClose()
                                },
                                onOrganize = onOrganize,
                                onOrganizeProject = onOrganizeProject,
                            )
                        }
                    }
                }
                index.looseProjects.forEach { project ->
                    ProjectBlock(
                        project, index, worktrees, expanded, selectedID,
                        onToggle = { id -> expanded = expanded.toggle(id) },
                        onNewSession = onNewSession,
                        onOpen = {
                            controller.openSession(it.id)
                            onClose()
                        },
                        onOrganize = onOrganize,
                        onOrganizeProject = onOrganizeProject,
                    )
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
    index: com.unpeel.controller.SidebarIndex,
    worktreesEnabled: Boolean,
    expanded: Set<String>,
    selectedID: String?,
    depth: Int = 0,
    onToggle: (String) -> Unit,
    onNewSession: (ProjectSummary) -> Unit,
    onOpen: (SessionSummary) -> Unit,
    onOrganize: (SessionSummary) -> Unit,
    onOrganizeProject: (ProjectSummary) -> Unit,
) {
    val open = project.id in expanded
    val rows = sidebarEntries(
        project,
        index.sessionsByProject[project.id].orEmpty(),
        index.childrenByParent[project.id].orEmpty(),
        index.hoistedSessionIDs,
        worktreesEnabled,
    )
    val attention = rows.any { entry ->
        when (entry) {
            is SidebarEntry.Session -> entry.session.activity == "blocked"
            is SidebarEntry.Project -> index.sessionsByProject[entry.project.id].orEmpty().any { it.activity == "blocked" }
        }
    }
    val tint = folderColorArgb(project.colorID)?.let { Color(it) } ?: Muted
    Row(
        Modifier
            .fillMaxWidth()
            .height(48.dp)
            .padding(start = (depth * 14).dp)
            .combinedClickable(onClick = { onToggle(project.id) }, onLongClick = { onOrganizeProject(project) }),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(if (open) "▾" else "▸", color = tint, fontSize = 16.sp, modifier = Modifier.width(22.dp))
        Text(
            project.name,
            color = if (project.isGroup) Foreground else Foreground.copy(alpha = 0.62f),
            fontSize = 16.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (attention) Box(Modifier.padding(end = 8.dp).size(8.dp).clip(CircleShape).background(Attention))
        if (project.pinned == true) Text("📌", fontSize = 12.sp, modifier = Modifier.padding(end = 4.dp))
        val branch = project.worktreeBranch ?: project.gitBranch
        if (!branch.isNullOrEmpty() && branch != project.name) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(end = 4.dp)) {
                BranchMark(12.dp, Muted.copy(alpha = 0.55f))
                Spacer(Modifier.width(3.dp))
                Text(branch, color = Muted.copy(alpha = 0.55f), fontSize = 12.sp, fontFamily = FontFamily.Monospace, maxLines = 1, modifier = Modifier.widthIn(max = 96.dp))
            }
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
        rows.forEach { entry ->
            when (entry) {
                is SidebarEntry.Session -> SessionLine(
                    entry.session,
                    entry.session.id == selectedID,
                    onOpen = { onOpen(entry.session) },
                    onOrganize = { onOrganize(entry.session) },
                )
                is SidebarEntry.Project -> ProjectBlock(
                    entry.project, index, worktreesEnabled, expanded, selectedID, depth + 1,
                    onToggle = onToggle,
                    onNewSession = onNewSession,
                    onOpen = onOpen,
                    onOrganize = onOrganize,
                    onOrganizeProject = onOrganizeProject,
                )
            }
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
        ToolLogo(
            command = session.command,
            providerID = session.providerID,
            activeRuntimeID = session.activeRuntimeID,
            running = session.status == "running",
            size = 28.dp,
        )
        Text(
            session.title.ifEmpty { session.command },
            color = if (session.status == "exited") Muted else Foreground,
            fontSize = 16.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f).padding(start = 10.dp, end = 8.dp),
        )
        if (session.notifyWhenDone) Text("🔔", fontSize = 12.sp, modifier = Modifier.padding(end = 6.dp))
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
private fun HostSheet(
    controller: Controller,
    lock: AppLock,
    onCredential: (Intent) -> Unit,
    onDismiss: () -> Unit,
) {
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
            Text("安全", color = Foreground, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("离开时锁定", color = Foreground, fontSize = 15.sp)
                    Text("指纹、面容，或锁屏密码", color = Muted, fontSize = 12.sp)
                }
                val activity = context as? Activity
                Switch(
                    checked = lock.enabled,
                    onCheckedChange = { armed ->
                        if (activity == null) return@Switch
                        if (armed) lock.arm(activity)?.let(onCredential) else lock.disarm()
                    },
                )
            }
            lock.lastError?.let {
                Text(it, color = Attention, fontSize = 12.sp, modifier = Modifier.padding(top = 4.dp))
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
                    else -> ToolLogo(session.command, session.providerID, session.activeRuntimeID, session.status == "running", 18.dp)
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
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    TextButton(
                        onClick = { controller.openArtifact(artifact.kind, artifact.name) },
                        modifier = Modifier.weight(1f),
                    ) {
                        Text("${artifact.kind}  ${artifact.name}", color = Foreground, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                    if (canPasteArtifact(artifact.kind, artifact.name)) {
                        TextButton(onClick = {
                            controller.pasteArtifact(artifact.kind, artifact.name)
                            onDismiss()
                        }) { Text("贴上", color = Foreground) }
                    }
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
    var notify by remember(session.id, session.notifyWhenDone) { mutableStateOf(session.notifyWhenDone) }
    var note by remember { mutableStateOf<String?>(null) }
    var confirmStop by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val offer = resumeOffer(session, controller.snapshot?.capabilities.orEmpty())
    val showNotify = canNotifyWhenDone(session)
    val showTranscript = AgentCatalog.supportsTranscript(session.providerID, session.command)
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 18.dp).verticalScroll(rememberScrollState())) {
            Text("编辑会话", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(10.dp))
            OutlinedTextField(value = title, onValueChange = { title = it }, singleLine = true, modifier = Modifier.fillMaxWidth())
            if (showNotify) {
                Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("完成后通知", color = Foreground, fontSize = 15.sp)
                        Text("这一轮结束时提醒你", color = Muted, fontSize = 12.sp)
                    }
                    Switch(checked = notify, onCheckedChange = { notify = it })
                }
            }
            Row {
                TextButton(
                    onClick = {
                        val trimmed = title.trim()
                        val titlePatch = trimmed.takeIf { it.isNotEmpty() && it != session.title }
                        val notifyPatch = notify.takeIf { showNotify && it != session.notifyWhenDone }
                        if (titlePatch == null && notifyPatch == null) {
                            onDismiss()
                            return@TextButton
                        }
                        controller.saveSession(session.id, titlePatch, null, notifyPatch) { failure ->
                            if (failure == null) onDismiss() else note = failure
                        }
                    },
                    enabled = title.isNotBlank(),
                ) { Text("保存") }
                TextButton(onClick = { controller.organize(session.id, pinned = !session.pinned) }) {
                    Text(if (session.pinned) "取消置顶" else "置顶")
                }
            }
            if (showTranscript) {
                Text("复制记录", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp))
                Row {
                    listOf(20 to "最近 20 条", 50 to "最近 50 条", 0 to "全部").forEach { (entries, label) ->
                        TextButton(onClick = {
                            controller.copyTranscript(session.id, entries) { markdown, failure ->
                                if (markdown != null) {
                                    val clipboard = context.getSystemService(android.content.ClipboardManager::class.java)
                                    clipboard?.setPrimaryClip(android.content.ClipData.newPlainText("transcript", markdown))
                                    note = "已复制为 Markdown"
                                } else {
                                    note = failure
                                }
                            }
                        }, enabled = !controller.busy) { Text(label, fontSize = 13.sp) }
                    }
                }
            }
            when (offer) {
                ResumeOffer.Agent -> TextButton(onClick = {
                    controller.sessionAction(session.id, "resume_agent")
                    onDismiss()
                }) { Text("恢复 Agent") }
                ResumeOffer.Session -> TextButton(onClick = {
                    controller.sessionAction(session.id, "restart")
                    onDismiss()
                }) { Text("恢复") }
                ResumeOffer.None -> Unit
            }
            if (canStopSession(session)) {
                TextButton(onClick = { confirmStop = true }) { Text("停止") }
            }
            TextButton(onClick = onRemove) { Text(if (session.status == "running") "移除会话" else "从列表移除", color = Attention) }
            note?.let { Text(it, color = if (it.startsWith("已复制")) Muted else Attention, fontSize = 12.sp) }
            Text(organizeHint(session, offer), color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp, bottom = 18.dp))
        }
    }
    if (confirmStop) {
        AlertDialog(
            onDismissRequest = { confirmStop = false },
            confirmButton = {
                TextButton(onClick = {
                    confirmStop = false
                    controller.sessionAction(session.id, "stop")
                    onDismiss()
                }) { Text("停止") }
            },
            dismissButton = { TextButton(onClick = { confirmStop = false }) { Text("取消") } },
            title = { Text("停止这个会话") },
            text = { Text("进程会停掉，会话留在列表里，之后可以恢复。") },
            containerColor = Sheet,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ProjectOrganizeSheet(controller: Controller, project: ProjectSummary, onDismiss: () -> Unit) {
    var name by remember(project.id) { mutableStateOf(project.name) }
    var colorID by remember(project.id) { mutableStateOf(project.colorID) }
    var dateSorted by remember(project.id) { mutableStateOf(project.dateSorted == true) }
    var note by remember { mutableStateOf<String?>(null) }
    val canEdit = controller.supportsProjectOrganization()
    val canRename = canEdit && project.isGroup
    val canColor = canEdit && !project.isGroup && project.parentProjectID == null
    val colors = listOf(null, "sky", "blue", "violet", "rose", "amber", "moss", "teal", "graphite")
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 18.dp).verticalScroll(rememberScrollState())) {
            Text(if (project.isGroup) "编辑分组" else "编辑文件夹", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(10.dp))
            if (!canEdit) {
                Text("请先在 Mac 上更新 Unpeel，才能在手机上整理文件夹。", color = Muted, fontSize = 14.sp)
            }
            if (canRename) {
                OutlinedTextField(value = name, onValueChange = { name = it }, singleLine = true, modifier = Modifier.fillMaxWidth())
            }
            if (canEdit) {
                Text("会话排序", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 12.dp, bottom = 4.dp))
                Row {
                    TextButton(onClick = { dateSorted = false }) { Text(if (!dateSorted) "自定义顺序 ✓" else "自定义顺序") }
                    TextButton(onClick = { dateSorted = true }) { Text(if (dateSorted) "按时间 ✓" else "按时间") }
                }
            }
            if (canColor) {
                Text("颜色", color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp, bottom = 6.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.horizontalScroll(rememberScrollState())) {
                    colors.forEach { id ->
                        val fill = folderColorArgb(id)?.let { Color(it) }
                        val selected = colorID == id
                        Box(
                            Modifier
                                .size(28.dp)
                                .clip(CircleShape)
                                .background(fill ?: Color.Transparent)
                                .border(2.dp, if (selected) Foreground else Hairline, CircleShape)
                                .clickable { colorID = id },
                        )
                    }
                }
            }
            if (canEdit) {
                TextButton(onClick = {
                    val trimmed = name.trim()
                    val namePatch = trimmed.takeIf { canRename && it.isNotEmpty() && it != project.name }
                    val colorPatch = when {
                        !canColor || colorID == project.colorID -> null
                        else -> colorID ?: ""
                    }
                    val sortPatch = dateSorted.takeIf { it != (project.dateSorted == true) }
                    if (namePatch == null && colorPatch == null && sortPatch == null) {
                        onDismiss()
                        return@TextButton
                    }
                    controller.saveProject(project.id, namePatch, colorPatch, sortPatch) { failure ->
                        if (failure == null) onDismiss() else note = failure
                    }
                }, enabled = !canRename || name.isNotBlank()) { Text("保存") }
            }
            note?.let { Text(it, color = Attention, fontSize = 12.sp) }
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
                    ToolLogo(preset.command, size = 22.dp)
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
private fun RoundKey(label: String, armed: Boolean = false, textSize: Int = 14, onClick: () -> Unit) {
    Box(
        Modifier
            .size(36.dp)
            .clip(CircleShape)
            .background(if (armed) Color(0xFF32ADE6) else Color.White.copy(alpha = 0.10f))
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            color = if (armed) Color.Black else Color.White.copy(alpha = 0.92f),
            fontSize = textSize.sp,
            fontWeight = FontWeight.SemiBold,
            fontFamily = if (label.length == 1) FontFamily.Monospace else FontFamily.Default,
        )
    }
}

@Composable
private fun KeyDot() {
    Box(Modifier.size(6.dp).clip(CircleShape).background(Color.White.copy(alpha = 0.28f)))
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

@Composable
private fun AppLockCover(lock: AppLock, onUnlock: () -> Unit) {
    Column(
        Modifier.fillMaxSize().background(Frame).clickable(onClick = {}).padding(28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("已锁定", color = Foreground, fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(8.dp))
        Text("验证后才能继续看会话", color = Muted, fontSize = 14.sp)
        lock.lastError?.let {
            Spacer(Modifier.height(8.dp))
            Text(it, color = Attention, fontSize = 13.sp)
        }
        Spacer(Modifier.height(18.dp))
        Button(onClick = onUnlock, enabled = !lock.authInFlight) { Text(if (lock.authInFlight) "正在验证…" else "解锁") }
    }
}

@Composable
private fun ExitedRestartBar(restarting: Boolean, onRestart: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 6.dp)
            .clip(RoundedCornerShape(22.dp))
            .background(Color.White.copy(alpha = 0.08f))
            .border(1.dp, Hairline, RoundedCornerShape(22.dp))
            .padding(start = 16.dp, end = 8.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(if (restarting) "正在恢复…" else "会话已结束", color = Foreground, fontSize = 14.sp, modifier = Modifier.weight(1f))
        TextButton(onClick = onRestart, enabled = !restarting) { Text("恢复") }
    }
}

@Composable
private fun FolderHeader(
    name: String,
    colorID: String?,
    open: Boolean,
    count: Int,
    attention: Boolean,
    onToggle: () -> Unit,
) {
    val tint = folderColorArgb(colorID)?.let { Color(it) } ?: Muted
    Row(
        Modifier.fillMaxWidth().height(36.dp).clickable(onClick = onToggle).padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(if (open) "▾" else "▸", color = tint, fontSize = 12.sp, modifier = Modifier.width(16.dp))
        Text(name.uppercase(), color = Foreground, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, modifier = Modifier.weight(1f))
        if (attention) Box(Modifier.padding(end = 6.dp).size(6.dp).clip(CircleShape).background(Attention))
        if (!open && count > 0) {
            Text(
                count.toString(),
                color = Muted,
                fontSize = 11.sp,
                modifier = Modifier.clip(RoundedCornerShape(9.dp)).background(Hover).padding(horizontal = 6.dp, vertical = 2.dp),
            )
        }
    }
}

private fun hostChrome(hue: Double?): Color {
    if (hue == null || hue.isNaN()) return Frame
    val wrapped = ((hue % 360.0) + 360.0) % 360.0
    return Color.hsl(wrapped.toFloat(), 0.18f, 0.10f)
}

private fun canPasteArtifact(kind: String, name: String): Boolean {
    if (kind == "screenshots" || kind == "computer") return true
    val lower = name.lowercase()
    return lower.endsWith(".png") || lower.endsWith(".jpg") || lower.endsWith(".jpeg") || lower.endsWith(".webp")
}

private fun organizeHint(session: SessionSummary, offer: ResumeOffer): String {
    val stop = canStopSession(session)
    return when {
        session.status == "running" && offer == ResumeOffer.Agent && stop ->
            "恢复 Agent 留在这个终端里。停止后会话还在列表中，可以再恢复。移除会删掉它。"
        session.status == "running" && offer == ResumeOffer.Agent ->
            "恢复 Agent 留在这个终端里。移除会停掉并删掉这个会话。"
        session.status == "running" && stop ->
            "停止后会话还在列表中，可以再恢复。移除会停掉并删掉它。"
        session.status == "running" -> "这次启动没有可恢复的记录。移除会停掉并删掉它。"
        offer == ResumeOffer.Session -> "恢复会接着这次对话。移除只是从列表里清掉。"
        else -> "移除只是从列表里清掉。"
    }
}

private fun Set<String>.toggle(id: String): Set<String> = if (id in this) this - id else this + id

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
