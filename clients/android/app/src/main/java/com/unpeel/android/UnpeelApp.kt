@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class, androidx.compose.material3.ExperimentalMaterial3Api::class)

package com.unpeel.android

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.net.Uri
import android.view.inputmethod.InputMethodManager
import androidx.activity.BackEventCompat
import androidx.activity.compose.BackHandler
import androidx.activity.compose.PredictiveBackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.input.pointer.changedToUpIgnoreConsumed
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalView
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
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
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.foundation.Canvas
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.zIndex
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextAlign
import androidx.core.content.ContextCompat
import com.unpeel.controller.AgentCatalog
import com.unpeel.controller.AgentTerminalPolicy
import com.unpeel.controller.PendingApproval
import com.unpeel.controller.ProjectSummary
import com.unpeel.controller.ResumeOffer
import com.unpeel.controller.SessionSummary
import com.unpeel.controller.SidebarEntry
import com.unpeel.controller.TerminalInput
import com.unpeel.controller.canNotifyWhenDone
import com.unpeel.controller.canStopSession
import com.unpeel.controller.folderColorArgb
import com.unpeel.controller.isInlineSidebarProject
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
import kotlin.coroutines.cancellation.CancellationException
import kotlin.math.abs
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
    // The session screen is the root. A system back swipe must not finish the
    // activity. The terminal screen replaces that gesture with the drawer.
    BackHandler { }
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
    var terminalSelecting by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val session = controller.snapshot?.sessions?.firstOrNull { it.id == controller.openSessionID }
    val pickFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            when (val loaded = withContext(Dispatchers.IO) { loadAttachment(context, uri) }) {
                is LoadedAttachment.Image -> annotate = loaded.bitmap
                is LoadedAttachment.File -> controller.attachFile(loaded.bytes, loaded.name)
                is LoadedAttachment.Failed -> controller.reportError(loaded.message)
            }
        }
    }
    fun chooseFile() {
        pickFile.launch(arrayOf("*/*"))
    }
    LaunchedEffect(controller.openSessionID) {
        terminalSelecting = false
        if (controller.openSessionID != null) {
            drawerDragging = false
            drawerOpen = false
        }
    }
    Box(Modifier.fillMaxSize().background(hostChrome(controller.snapshot?.hostTintHue))) {
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val drawerWidth = minOf(360.dp, maxOf(286.dp, maxWidth * 0.88f))
        val drawerPx = with(LocalDensity.current) { drawerWidth.toPx() }.coerceAtLeast(1f)
        val revealCommitPx = with(LocalDensity.current) { 90.dp.toPx() }
        val view = LocalView.current
        val allowReveal = rememberUpdatedState(sheet == null && !locked && !drawerOpen && !terminalSelecting)
        // iOS opens the sidebar from a rightward drag in the left 15%. Android's
        // system back is that same swipe and would finish this root activity.
        PredictiveBackHandler(enabled = !locked && sheet == null) { progress ->
            if (terminalSelecting) {
                progress.collect { }
                return@PredictiveBackHandler
            }
            val wasOpen = drawerOpen
            var fromLeft = !wasOpen
            try {
                progress.collect { event ->
                    fromLeft = event.swipeEdge == BackEventCompat.EDGE_LEFT
                    drawerDragging = true
                    when {
                        wasOpen -> drawerPosition = drawerPx * (1f - event.progress.coerceIn(0f, 1f))
                        fromLeft -> drawerPosition = event.touchX.coerceIn(0f, drawerPx)
                    }
                }
                drawerDragging = false
                when {
                    wasOpen -> drawerOpen = false
                    fromLeft -> {
                        drawerOpen = true
                        view.context.getSystemService(InputMethodManager::class.java)
                            ?.hideSoftInputFromWindow(view.windowToken, 0)
                    }
                    else -> drawerOpen = false
                }
            } catch (_: CancellationException) {
                drawerDragging = false
                drawerOpen = wasOpen
            }
        }
        LaunchedEffect(drawerOpen, drawerDragging, drawerPx) {
            if (drawerDragging) return@LaunchedEffect
            val target = if (drawerOpen) drawerPx else 0f
            if (drawerPosition == target) return@LaunchedEffect
            animate(drawerPosition, target, animationSpec = tween(220)) { value, _ -> drawerPosition = value }
        }
        fun closeBy(totalX: Float) {
            drawerDragging = true
            drawerPosition = (drawerPx + totalX).coerceIn(0f, drawerPx)
        }
        fun releaseClose(dismiss: Boolean) {
            drawerDragging = false
            drawerOpen = !dismiss
        }
        Box(
            Modifier
                .fillMaxSize()
                .revealSessionsDrawer(
                    allow = { allowReveal.value },
                    commitPx = revealCommitPx,
                    onMove = { total ->
                        drawerDragging = true
                        drawerPosition = total.coerceIn(0f, drawerPx)
                    },
                    onFinish = { total ->
                        drawerDragging = false
                        if (total > revealCommitPx) {
                            drawerOpen = true
                            view.context.getSystemService(InputMethodManager::class.java)
                                ?.hideSoftInputFromWindow(view.windowToken, 0)
                        } else {
                            drawerOpen = false
                        }
                    },
                ),
        ) {
            if (session == null) {
                EmptyTerminal(disconnected = controller.snapshot == null, hostName = controller.selectedHost?.macName) {
                    drawerOpen = true
                }
            } else {
                TerminalSurface(
                    controller,
                    session,
                    onSessions = { drawerOpen = true },
                    onSheet = { kind ->
                        if (kind == HomeSheet.Organize) organizeID = session.id
                        sheet = kind
                    },
                    onSelectionChanged = { terminalSelecting = it },
                )
            }
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
                    onDrag = ::closeBy,
                    onRelease = ::releaseClose,
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
        LinkNotice(controller.connection)
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
        HomeSheet.Gallery -> GallerySheet(controller, onPickFile = ::chooseFile) { sheet = null }
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
    onSheet: (HomeSheet) -> Unit,
    onSelectionChanged: (Boolean) -> Unit = {},
) {
    val project = controller.snapshot?.projects?.firstOrNull { it.id == session.projectID }
    var ctrlArmed by remember { mutableStateOf(false) }
    var terminalView by remember { mutableStateOf<HostTerminalView?>(null) }
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner, session.id) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_START || event == Lifecycle.Event.ON_RESUME || event == Lifecycle.Event.ON_STOP) {
                terminalView?.dismissKeyboard()
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            terminalView?.dismissKeyboard()
        }
    }
    DisposableEffect(session.id) { onDispose { controller.releasePhoneFit() } }
    // Hidden per session. A new session shows its title again. The bar leaves
    // the layout when it collapses, so the phone grid picks up the extra rows
    // once the height settles. A swipe on the terminal itself stays scroll.
    var chromeVisible by remember(session.id) { mutableStateOf(true) }
    Column(Modifier.fillMaxSize()) {
        AnimatedVisibility(
            visible = chromeVisible,
            enter = expandVertically(animationSpec = tween(180)) + fadeIn(tween(180)),
            exit = shrinkVertically(animationSpec = tween(180)) + fadeOut(tween(120)),
        ) {
        Row(
            Modifier
                .fillMaxWidth()
                .collapseWhenSwipedUp { chromeVisible = false }
                .padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 6.dp),
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
        }
        TerminalGrid(
            controller,
            onView = { terminalView = it },
            controlArmed = { ctrlArmed },
            onControlUsed = { ctrlArmed = false },
            revealChrome = !chromeVisible,
            onRevealChrome = { chromeVisible = true },
            onSelectionChanged = onSelectionChanged,
        )
        Column(Modifier.windowInsetsPadding(WindowInsets.ime.union(WindowInsets.navigationBars))) {
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
        val keyboardOpen = WindowInsets.ime.getBottom(LocalDensity.current) > 0
        if (keyboardOpen) {
            Row(
                Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 6.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TerminalKey("◀") { sendTerminalKey(controller, "arrowLeft", ctrlArmed); ctrlArmed = false }
                TerminalKey("▲") { sendTerminalKey(controller, "arrowUp", ctrlArmed); ctrlArmed = false }
                TerminalKey("▼") { sendTerminalKey(controller, "arrowDown", ctrlArmed); ctrlArmed = false }
                TerminalKey("▶") { sendTerminalKey(controller, "arrowRight", ctrlArmed); ctrlArmed = false }
                TerminalKey("esc", 11) { sendTerminalKey(controller, "escape", ctrlArmed); ctrlArmed = false }
                TerminalKey("Tab", 12) { sendTerminalKey(controller, "tab", ctrlArmed); ctrlArmed = false }
                TerminalKey("Ctrl", 12, armed = ctrlArmed) { ctrlArmed = !ctrlArmed }
            }
        }
        }
    }
}

/**
 * iOS `acceptsSidebarReveal`: a rightward drag that starts in the left 15% of
 * the session and is clearly horizontal opens the drawer. Anything else is
 * left for the terminal, including vertical scroll and text selection.
 */
@Composable
private fun Modifier.revealSessionsDrawer(
    allow: () -> Boolean,
    commitPx: Float,
    onMove: (Float) -> Unit,
    onFinish: (Float) -> Unit,
): Modifier {
    val allowState = rememberUpdatedState(allow)
    val moveState = rememberUpdatedState(onMove)
    val finishState = rememberUpdatedState(onFinish)
    return pointerInput(commitPx) {
        val slop = 18.dp.toPx()
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
            if (!allowState.value() || down.position.x > size.width * 0.15f) return@awaitEachGesture
            var totalX = 0f
            var totalY = 0f
            var claimed = false
            var last = down.position
            while (true) {
                val event = awaitPointerEvent(PointerEventPass.Initial)
                val change = event.changes.firstOrNull { it.id == down.id } ?: break
                totalX += change.position.x - last.x
                totalY += change.position.y - last.y
                last = change.position
                if (!claimed) {
                    val pastSlop = abs(totalX) >= slop || abs(totalY) >= slop
                    if (!pastSlop) {
                        if (!change.pressed) break
                        continue
                    }
                    val horizontal = totalX > 0f && abs(totalX) > abs(totalY) * 1.3f
                    if (!horizontal || !allowState.value()) return@awaitEachGesture
                    claimed = true
                }
                moveState.value(totalX)
                change.consume()
                if (!change.pressed) {
                    finishState.value(totalX)
                    break
                }
            }
        }
    }
}

/**
 * iOS `closeDrag`: a horizontal drag on the open drawer follows the finger.
 * The first move past 12dp decides once. Vertical stays a list scroll. A
 * leftward drag of 80dp, or a flick that would carry 180dp, dismisses.
 * Anything shorter springs back open.
 */
@Composable
private fun Modifier.closeSessionsDrawer(
    onMove: (Float) -> Unit,
    onFinish: (Boolean) -> Unit,
    onSlideLock: (Boolean) -> Unit,
): Modifier {
    val moveState = rememberUpdatedState(onMove)
    val finishState = rememberUpdatedState(onFinish)
    val lockState = rememberUpdatedState(onSlideLock)
    return pointerInput(Unit) {
        val slop = 12.dp.toPx()
        val dismissPx = 80.dp.toPx()
        val predictedPx = 180.dp.toPx()
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
            val tracker = VelocityTracker()
            tracker.addPosition(down.uptimeMillis, Offset.Zero)
            var totalX = 0f
            var totalY = 0f
            var decided = false
            var slide = false
            var finished = false
            try {
                while (true) {
                    val event = awaitPointerEvent(PointerEventPass.Initial)
                    val change = event.changes.firstOrNull { it.id == down.id } ?: break
                    // position is local to the drawer, and the drawer moves
                    // under the finger. previousPosition is already in this
                    // event's coordinates, so the difference is the finger,
                    // not the drawer's own shift. Subtracting a stored point
                    // from the last event made a slow drag shake left and right.
                    val previous = change.previousPosition
                    if (previous != Offset.Unspecified) {
                        totalX += change.position.x - previous.x
                        totalY += change.position.y - previous.y
                    }
                    tracker.addPosition(change.uptimeMillis, Offset(totalX, totalY))
                    if (!decided) {
                        if (abs(totalX) < slop && abs(totalY) < slop) {
                            if (!change.pressed) break
                            continue
                        }
                        decided = true
                        slide = abs(totalX) > abs(totalY)
                        if (!slide) break
                        lockState.value(true)
                    }
                    moveState.value(kotlin.math.min(0f, totalX))
                    change.consume()
                    if (!change.pressed) {
                        val predicted = totalX + tracker.calculateVelocity().x * 0.15f
                        finished = true
                        finishState.value(totalX < -dismissPx || predicted < -predictedPx)
                        break
                    }
                }
            } finally {
                if (slide) {
                    lockState.value(false)
                    if (!finished) finishState.value(false)
                }
            }
        }
    }
}

/**
 * Up-swipe hides the session bar. The drag has to win over a tap, and it has
 * to be more vertical than horizontal, so the menu buttons and the drawer
 * edge still get their own gestures. The terminal below this bar is not a
 * collapse target: that drag is scrollback.
 */
@Composable
private fun Modifier.collapseWhenSwipedUp(onCollapse: () -> Unit): Modifier {
    val current = rememberUpdatedState(onCollapse)
    return pointerInput(Unit) {
        val threshold = 32.dp.toPx()
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false)
            val origin = down.position
            var totalY = 0f
            var fired = false
            while (true) {
                val event = awaitPointerEvent()
                val change = event.changes.firstOrNull { it.id == down.id } ?: break
                if (!change.pressed) break
                totalY += change.position.y - change.previousPosition.y
                val dx = change.position.x - origin.x
                if (!fired && totalY < -threshold && kotlin.math.abs(totalY) > kotlin.math.abs(dx)) {
                    fired = true
                    change.consume()
                    current.value()
                } else if (fired) {
                    change.consume()
                }
            }
        }
    }
}

/**
 * The hidden bar comes back from this strip only. A downward swipe on the
 * rest of the terminal stays scrollback, pinch, or a cursor drag.
 */
@Composable
private fun Modifier.restoreSessionChrome(onReveal: () -> Unit): Modifier {
    val current = rememberUpdatedState(onReveal)
    return pointerInput(Unit) {
        val slop = viewConfiguration.touchSlop
        val threshold = 20.dp.toPx()
        awaitEachGesture {
            val down = awaitFirstDown()
            val origin = down.position
            var totalY = 0f
            var fired = false
            while (true) {
                val event = awaitPointerEvent()
                val change = event.changes.firstOrNull { it.id == down.id } ?: break
                if (change.changedToUpIgnoreConsumed()) {
                    val dx = change.position.x - origin.x
                    val dy = change.position.y - origin.y
                    if (!fired && dx * dx + dy * dy <= slop * slop) current.value()
                    break
                }
                if (!change.pressed) break
                totalY += change.position.y - change.previousPosition.y
                change.consume()
                if (!fired && totalY > threshold) {
                    fired = true
                    current.value()
                }
            }
        }
    }
}

@Composable
private fun ColumnScope.TerminalGrid(
    controller: Controller,
    onView: (HostTerminalView) -> Unit,
    controlArmed: () -> Boolean,
    onControlUsed: () -> Unit,
    revealChrome: Boolean = false,
    onRevealChrome: () -> Unit = {},
    onSelectionChanged: (Boolean) -> Unit = {},
) {
    val density = LocalDensity.current
    val imeBottom = WindowInsets.ime.getBottom(density)
    var gridSize by remember(controller.openSessionID) { mutableStateOf(IntSize.Zero) }
    var cellW by remember { mutableFloatStateOf(0f) }
    var cellH by remember { mutableFloatStateOf(0f) }
    var topPad by remember { mutableFloatStateOf(0f) }
    // One flag for the life of this grid. Keying it on the session left the
    // view callback writing the first session's state, so later sessions
    // never showed Copy.
    var selecting by remember { mutableStateOf(false) }
    var menuActive by remember { mutableStateOf(false) }
    var surface by remember { mutableStateOf<HostTerminalView?>(null) }
    val openSession = controller.snapshot?.sessions?.firstOrNull { it.id == controller.openSessionID }
    val canvas = terminalCanvas(openSession?.terminalBackgroundHex)
    val shownTerminal = controller.terminal
    LaunchedEffect(controller.openSessionID) {
        surface?.clearSelection()
        selecting = false
        onSelectionChanged(false)
    }
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
            // The embedded Android view draws above Compose. Without this clip it
            // covers the title row and that band is just the terminal color.
            .clipToBounds()
            .background(Color(canvas)),
    ) {
        AndroidView(
            modifier = Modifier.fillMaxSize().onSizeChanged { gridSize = it }.focusProperties { canFocus = false },
            factory = { context ->
                HostTerminalView(context, controller.terminal).also { view ->
                    surface = view
                    onView(view)
                    cellW = view.cellWidth
                    cellH = view.cellHeight.toFloat()
                    topPad = view.topPad.toFloat()
                    view.onSelectionChanged = {
                        selecting = it
                        onSelectionChanged(it)
                    }
                }
            },
            update = { view ->
                surface = view
                view.onSelectionChanged = {
                    selecting = it
                    onSelectionChanged(it)
                }
                view.onMenuChanged = { active ->
                    if (active && !menuActive) view.dismissKeyboard()
                    menuActive = active
                }
                view.relayed = controller.usingRelay
                view.prefersRemoteWheel = AgentTerminalPolicy.prefersRemoteMouseWheel(
                    openSession?.providerID,
                    openSession?.command.orEmpty(),
                    openSession?.activeRuntimeID,
                )
                shownTerminal.applyCanvas(canvas)
                view.canvasColor = canvas
                view.bind(shownTerminal)
                view.onText = { controller.sendRaw(it) }
                view.controlArmed = controlArmed
                view.onControlUsed = onControlUsed
                onView(view)
            },
        )
        if (revealChrome && !selecting && !controller.sessionPreparing) {
            Box(
                Modifier
                    .align(Alignment.TopCenter)
                    .zIndex(1f)
                    .fillMaxWidth()
                    .height(28.dp)
                    .restoreSessionChrome(onRevealChrome),
                contentAlignment = Alignment.Center,
            ) {
                Box(
                    Modifier
                        .width(40.dp)
                        .height(4.dp)
                        .clip(RoundedCornerShape(2.dp))
                        .background(Color.Black.copy(alpha = 0.45f))
                        .border(1.dp, Color.White.copy(alpha = 0.72f), RoundedCornerShape(2.dp)),
                )
            }
        }
        if (selecting) {
            Row(
                Modifier
                    .align(Alignment.TopCenter)
                    .zIndex(2f)
                    .padding(8.dp)
                    .background(Color(0xF01A1B1D), RoundedCornerShape(18.dp))
                    .border(1.dp, Color.White.copy(alpha = 0.12f), RoundedCornerShape(18.dp)),
            ) {
                TextButton(onClick = {
                    if (surface?.copySelection() != true) controller.reportError("没有可复制的文字")
                }) { Text("复制", color = Foreground) }
                TextButton(onClick = { surface?.clearSelection() }) { Text("取消", color = Muted) }
            }
        }
        if (menuActive && imeBottom == 0) {
            Row(
                Modifier.align(Alignment.BottomCenter).zIndex(2f).padding(bottom = 10.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TerminalKey("▲") { surface?.menuKey("up")?.let(controller::sendRaw) }
                TerminalKey("▼") { surface?.menuKey("down")?.let(controller::sendRaw) }
                TerminalKey("esc", 11) { surface?.menuKey("escape")?.let(controller::sendRaw) }
                TerminalKey("⏎") { surface?.menuKey("enter")?.let(controller::sendRaw) }
            }
        }
        if (controller.sessionPreparing) {
            TerminalPrepareCover(canvas, Modifier.matchParentSize().zIndex(4f))
        }
    }
}

/**
 * Same job as iOS `TerminalSkeletonView`: an opaque terminal-shaped cover
 * until the first replay has painted. The caption is the part the shimmer
 * alone does not say.
 */
@Composable
private fun TerminalPrepareCover(canvas: Int, modifier: Modifier = Modifier) {
    val pulse = rememberInfiniteTransition(label = "prepare")
    val alpha by pulse.animateFloat(
        initialValue = 1f,
        targetValue = 0.55f,
        animationSpec = infiniteRepeatable(tween(900), RepeatMode.Reverse),
        label = "prepareAlpha",
    )
    val fractions = floatArrayOf(
        0.82f, 0.58f, 0.71f, 0.36f, 0.88f, 0.52f,
        0.28f, 0.64f, 0.47f, 0.76f, 0.41f, 0.60f,
    )
    BoxWithConstraints(
        modifier
            .fillMaxSize()
            .background(Color(canvas))
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent()
                        event.changes.forEach { it.consume() }
                    }
                }
            },
    ) {
        val lineWidth = (maxWidth - 64.dp).coerceAtLeast(0.dp)
        Column(Modifier.padding(start = 32.dp, end = 32.dp, top = 22.dp).graphicsLayer { this.alpha = alpha }) {
            Text("正在准备", color = Color.White.copy(alpha = 0.72f), fontSize = 13.sp)
            Spacer(Modifier.height(18.dp))
            fractions.forEach { fraction ->
                Box(
                    Modifier
                        .padding(bottom = 13.dp)
                        .width(lineWidth * fraction)
                        .height(12.dp)
                        .background(Color.White.copy(alpha = 0.07f)),
                )
            }
        }
    }
}

private fun terminalCanvas(hex: Int?): Int {
    if (hex == null) return HostTerminal.CANVAS
    return 0xFF000000.toInt() or (hex and 0xFFFFFF)
}

/**
 * Columns and rows that letterbox the Host PTY to this terminal box.
 * Null until the box can hold a real phone grid. A first layout pass of a
 * few dozen pixels must not become the PTY width: at two columns the
 * program wraps one glyph per line, and that wrap stays in the scrollback.
 */
internal fun phoneFitGrid(widthPx: Int, heightPx: Int, cellW: Float, cellH: Float, topPadPx: Float): Pair<Int, Int>? {
    val innerH = heightPx - topPadPx
    if (cellW <= 0f || cellH <= 0f) return null
    val columns = (widthPx / cellW).toInt()
    val rows = (innerH / cellH).toInt()
    if (columns < 20 || rows < 8) return null
    return columns.coerceAtMost(300) to rows.coerceAtMost(120)
}

@Composable
private fun SessionDrawer(
    controller: Controller,
    selectedID: String?,
    width: Dp,
    onDrag: (Float) -> Unit,
    onRelease: (Boolean) -> Unit,
    onClose: () -> Unit,
    onWorkspaces: () -> Unit,
    onNewSession: (ProjectSummary) -> Unit,
    onOrganize: (SessionSummary) -> Unit,
    onOrganizeProject: (ProjectSummary) -> Unit,
) {
    val snapshot = controller.snapshot
    val listScroll = rememberScrollState()
    var slideLock by remember { mutableStateOf(false) }
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
    Box(
        Modifier
            .fillMaxHeight()
            .width(width)
            .shadow(22.dp, RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp), clip = false)
            .clip(RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp))
            .background(hostChrome(snapshot?.hostTintHue))
            .closeSessionsDrawer(
                onMove = onDrag,
                onFinish = onRelease,
                onSlideLock = { slideLock = it },
            ),
    ) {
    Column(Modifier.fillMaxSize()) {
        WorkspaceHeader(controller, onWorkspaces)
        Column(
            Modifier
                .weight(1f)
                .sidebarListFade()
                .verticalScroll(listScroll, enabled = !slideLock)
                .padding(start = 8.dp, end = 8.dp, top = 16.dp, bottom = 40.dp),
        ) {
            if (snapshot == null) {
                SidebarDisconnected(controller.connection)
            } else {
                val index = sidebarIndex(snapshot)
                val approvalIDs = snapshot.pendingApprovals.mapNotNull { it.targetSessionID ?: it.callerSessionID }.toSet()
                val tintHue = snapshot.hostTintHue
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
                        attention = !open && memberSessions.any { it.activity == "blocked" || it.id in approvalIDs },
                        onToggle = { openFolder = if (open) null else group.folder.id },
                    )
                    if (open) {
                        group.projects.forEach { project ->
                            ProjectBlock(
                                project, index, worktrees, expanded, selectedID, approvalIDs, tintHue,
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
                        project, index, worktrees, expanded, selectedID, approvalIDs, tintHue,
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
        SidebarFeedbackFooter()
    }
        Box(
            Modifier
                .align(Alignment.CenterEnd)
                .fillMaxHeight()
                .width(1.dp)
                .background(Color.White.copy(alpha = 0.10f)),
        )
    }
}

/** A quiet top line. Connecting to LAN or the public address shows it, and so does a switch, because that is another connection. It leaves on its own. */
@Composable
private fun BoxScope.LinkNotice(connection: String) {
    val text = when (connection) {
        "局域网" -> "已连接局域网"
        "公网" -> "已连接公网"
        else -> null
    }
    var visible by remember { mutableStateOf(false) }
    LaunchedEffect(text) {
        if (text == null) {
            visible = false
            return@LaunchedEffect
        }
        visible = true
        delay(5_000)
        visible = false
    }
    if (!visible || text == null) return
    Text(
        text,
        color = Color.White.copy(alpha = 0.92f),
        fontSize = 13.sp,
        fontWeight = FontWeight.Medium,
        modifier = Modifier
            .align(Alignment.TopCenter)
            .padding(top = 10.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Color(0xCC1C1C1A))
            .padding(horizontal = 14.dp, vertical = 8.dp),
    )
}

/**
 * Pinned drawer header, the same row as iOS `SidebarWorkspaceHeader`.
 * The workspace name leads. A healthy direct connection stays quiet.
 * Tap opens the workspace sheet. There is no separate Host button.
 */
@Composable
private fun WorkspaceHeader(controller: Controller, onClick: () -> Unit) {
    val snapshot = controller.snapshot
    val current = snapshot?.workspaces?.firstOrNull { it.isCurrent } ?: snapshot?.workspaces?.firstOrNull()
    val title = current?.name?.takeIf { it.isNotBlank() }
        ?: snapshot?.macName
        ?: controller.selectedHost?.macName
        ?: "工作区"
    val failing = snapshot == null && controller.connection != "正在连接" && controller.connection != "未配对"
    val subtitle = when {
        snapshot == null -> when (controller.connection) {
            "未配对" -> "未配对"
            "正在连接" -> "正在连接…"
            else -> "连接中断"
        }
        controller.usingRelay -> "经由 Unpeel Remote"
        else -> null
    }
    Row(
        Modifier
            .padding(start = 8.dp, end = 8.dp, top = 14.dp)
            .clip(RoundedCornerShape(9.dp))
            .background(Hover)
            .clickable(onClick = onClick)
            .padding(start = 9.dp, end = 9.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (failing) {
            WifiSlash(16.dp, Attention)
        } else {
            Box(Modifier.size(16.dp), contentAlignment = Alignment.Center) {
                WorkspaceTintDot(current?.tintHue)
            }
        }
        Spacer(Modifier.width(7.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    title,
                    color = Foreground,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                when (current?.kind) {
                    "ssh" -> {
                        Spacer(Modifier.width(5.dp))
                        SvgIcon("terminal", 9.dp, tint = Muted)
                    }
                    "paired" -> {
                        Spacer(Modifier.width(5.dp))
                        ServerMark(9.dp, Muted)
                    }
                }
            }
            if (subtitle != null) {
                Text(subtitle, color = Muted, fontSize = 10.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        if (snapshot != null) {
            Spacer(Modifier.width(6.dp))
            Box(
                Modifier
                    .clip(RoundedCornerShape(percent = 50))
                    .background(Hover)
                    .height(18.dp)
                    .padding(horizontal = 6.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    snapshot.sessions.size.toString(),
                    color = Muted,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.Medium,
                )
            }
        }
        Spacer(Modifier.width(6.dp))
        ChevronUpDown(Muted)
    }
}

@Composable
private fun ProjectBlock(
    project: ProjectSummary,
    index: com.unpeel.controller.SidebarIndex,
    worktreesEnabled: Boolean,
    expanded: Set<String>,
    selectedID: String?,
    approvalIDs: Set<String>,
    tintHue: Double?,
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
    fun sessionNeedsAttention(session: SessionSummary) =
        session.activity == "blocked" || session.id in approvalIDs
    val attention = rows.any { entry ->
        when (entry) {
            is SidebarEntry.Session -> sessionNeedsAttention(entry.session)
            is SidebarEntry.Project -> index.sessionsByProject[entry.project.id].orEmpty().any(::sessionNeedsAttention)
        }
    }
    val tint = folderColorArgb(project.colorID)?.let { Color(it) } ?: Muted
    val child = isInlineSidebarProject(project)
    val leading = if (child) (18 + (depth - 1).coerceAtLeast(0) * 14).dp else (7 + depth * 14).dp
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 32.dp)
            .padding(start = leading, top = 2.dp, end = 7.dp, bottom = 2.dp)
            .combinedClickable(onClick = { onToggle(project.id) }, onLongClick = { onOrganizeProject(project) }),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(if (child) 12.dp else 22.dp), contentAlignment = Alignment.Center) {
            if (child) {
                SidebarChevron(open, tint, 12.dp)
            } else if (!SvgIcon(if (open) "folder-open" else "folder-closed", 18.dp, tint = tint)) {
                SidebarChevron(open, tint, 12.dp)
            }
            if (attention) {
                Box(
                    Modifier.align(Alignment.TopEnd).offset(x = 1.dp, y = (-1).dp).size(14.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Box(Modifier.size(14.dp).clip(CircleShape).background(Attention.copy(alpha = 0.20f)))
                    Box(Modifier.size(6.dp).clip(CircleShape).background(Attention))
                }
            }
        }
        Spacer(Modifier.width(7.dp))
        // Name, pin, and branch stay together on the left. Only this cluster
        // is flexible, so the add button keeps one trailing edge on every row.
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
            Text(
                project.name,
                color = if (project.isGroup) Foreground else Foreground.copy(alpha = 0.62f),
                fontSize = 14.sp,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (project.pinned == true) {
                Spacer(Modifier.width(4.dp))
                if (!SvgIcon("pin", 11.dp, tint = Muted)) Text("📌", fontSize = 11.sp)
            }
            val branch = project.worktreeBranch ?: project.gitBranch
            if (!branch.isNullOrEmpty() && branch != project.name) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(start = 4.dp)) {
                    BranchMark(12.dp, Muted.copy(alpha = 0.55f))
                    Spacer(Modifier.width(3.dp))
                    Text(
                        branch,
                        color = Muted.copy(alpha = 0.55f),
                        fontSize = 10.sp,
                        fontFamily = FontFamily.Monospace,
                        maxLines = 1,
                        modifier = Modifier.widthIn(max = 104.dp),
                    )
                }
            }
        }
        if (!project.isGroup) {
            Spacer(Modifier.width(4.dp))
            Box(
                Modifier
                    .size(28.dp)
                    .clip(RoundedCornerShape(9.dp))
                    .background(Hover.copy(alpha = Hover.alpha * 0.44f))
                    .clickable { onNewSession(project) },
                contentAlignment = Alignment.Center,
            ) {
                if (!SvgIcon("plus", 17.dp, tint = Muted)) Text("+", color = Muted, fontSize = 16.sp)
            }
        }
    }
    if (open) {
        rows.forEach { entry ->
            when (entry) {
                is SidebarEntry.Session -> SessionLine(
                    entry.session,
                    entry.session.id == selectedID,
                    needsAttention = sessionNeedsAttention(entry.session),
                    tintHue = tintHue,
                    depth = depth + 1,
                    onOpen = { onOpen(entry.session) },
                    onOrganize = { onOrganize(entry.session) },
                )
                is SidebarEntry.Project -> ProjectBlock(
                    entry.project, index, worktreesEnabled, expanded, selectedID, approvalIDs, tintHue, depth + 1,
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
private fun SessionLine(
    session: SessionSummary,
    selected: Boolean,
    needsAttention: Boolean,
    tintHue: Double?,
    depth: Int,
    onOpen: () -> Unit,
    onOrganize: () -> Unit,
) {
    val blocked = session.status == "running" && (needsAttention || session.activity == "blocked")
    Row(
        Modifier
            .fillMaxWidth()
            .padding(start = (11 + (depth - 1).coerceAtLeast(0) * 14).dp, top = 2.dp, end = 9.dp, bottom = 2.dp)
            .heightIn(min = 32.dp)
            .clip(RoundedCornerShape(9.dp))
            .background(if (selected) selectedRowTint(tintHue) else Color.Transparent)
            .combinedClickable(onClick = onOpen, onLongClick = onOrganize)
            .padding(vertical = 2.dp)
            .graphicsLayer { alpha = if (session.status == "exited") 0.82f else 1f },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(18.dp), contentAlignment = Alignment.Center) {
            when {
                blocked -> {
                    Box(Modifier.size(14.dp).clip(CircleShape).background(Attention.copy(alpha = 0.20f)))
                    Box(Modifier.size(6.dp).clip(CircleShape).background(Attention))
                }
                session.status == "running" && session.activity == "working" ->
                    BrailleSpinner(rgb(session.spinnerColorHex ?: 0xB9BDC9))
                session.unread -> Box(Modifier.size(7.dp).clip(CircleShape).background(Unread))
            }
        }
        Spacer(Modifier.width(8.dp))
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
            Text(
                session.title.ifEmpty { session.command },
                color = if (session.status == "exited") Muted else Foreground,
                fontSize = 14.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (session.pinned) {
                Spacer(Modifier.width(4.dp))
                if (!SvgIcon("pin", 11.dp, tint = Muted)) Text("📌", fontSize = 11.sp)
            }
        }
        Spacer(Modifier.width(4.dp))
        if (session.notifyWhenDone) {
            if (!SvgIcon("bell", 11.dp, tint = Muted)) Text("🔔", fontSize = 11.sp)
            Spacer(Modifier.width(4.dp))
        }
        Text(
            if (blocked) "blocked" else relativeAge(session.updatedAtUnixMs),
            color = Muted,
            fontSize = 10.sp,
            fontWeight = if (selected) FontWeight.Medium else FontWeight.Normal,
            modifier = Modifier.widthIn(min = 24.dp),
            textAlign = TextAlign.End,
        )
        Spacer(Modifier.width(5.dp))
        Box(Modifier.size(14.dp), contentAlignment = Alignment.Center) {
            ToolLogo(
                command = session.command,
                providerID = session.providerID,
                activeRuntimeID = session.activeRuntimeID,
                running = session.status == "running",
                size = 12.dp,
            )
        }
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
            Text("工作区", color = Foreground, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            Text("每个配对是一个工作区。点一行切换，下面可以再配一台。", color = Muted, fontSize = 13.sp)
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
                Text("这台 Mac 上的工作区", color = Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
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
private fun GallerySheet(controller: Controller, onPickFile: () -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Sheet) {
        Column(Modifier.padding(horizontal = 16.dp).verticalScroll(rememberScrollState())) {
            Text("截图和图库", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            TextButton(onClick = controller::requestScreenshot) { Text("现在截一张", color = Foreground) }
            TextButton(onClick = {
                onPickFile()
                onDismiss()
            }) { Text("附上文件", color = Foreground) }
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
    var lastCode by remember { mutableStateOf<String?>(null) }
    var lastCodeAt by remember { mutableStateOf(0L) }
    var cameraGranted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED,
        )
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        cameraGranted = granted
        if (!granted) controller.reportError("需要相机权限才能扫配对码")
    }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text(
            "扫描配对码",
            color = Color.White.copy(alpha = 0.85f),
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.fillMaxWidth(),
            textAlign = TextAlign.Start,
        )
        Box(
            Modifier
                .fillMaxWidth()
                .height(240.dp)
                .clip(RoundedCornerShape(14.dp))
                .background(Color.Black),
            contentAlignment = Alignment.Center,
        ) {
            if (cameraGranted) {
                PairingScanner(enabled = !controller.busy) { scanned ->
                    val now = android.os.SystemClock.elapsedRealtime()
                    if (controller.busy || (scanned == lastCode && now - lastCodeAt < 2_500)) return@PairingScanner
                    lastCode = scanned
                    lastCodeAt = now
                    controller.pair(scanned)
                }
            } else {
                Text(
                    "需要相机权限才能扫配对码",
                    color = Color.White.copy(alpha = 0.7f),
                    fontSize = 14.sp,
                    textAlign = TextAlign.Center,
                    modifier = Modifier
                        .fillMaxSize()
                        .clickable { permission.launch(Manifest.permission.CAMERA) }
                        .padding(24.dp),
                )
            }
        }
        Text(
            "或粘贴配对码",
            color = Color.White.copy(alpha = 0.85f),
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = code,
            onValueChange = { code = it },
            modifier = Modifier.fillMaxWidth().heightIn(min = 72.dp),
            placeholder = { Text("配对码", color = Color.White.copy(alpha = 0.35f), fontSize = 12.sp) },
            minLines = 3,
            textStyle = TextStyle(
                color = Foreground,
                fontSize = 12.sp,
                fontFamily = FontFamily.Monospace,
            ),
            shape = RoundedCornerShape(10.dp),
            colors = OutlinedTextFieldDefaults.colors(
                focusedBorderColor = Color.Transparent,
                unfocusedBorderColor = Color.Transparent,
                focusedContainerColor = Color.White.copy(alpha = 0.06f),
                unfocusedContainerColor = Color.White.copy(alpha = 0.06f),
                cursorColor = Color(0xFF32ADE6),
            ),
        )
        Box(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(12.dp))
                .background(if (!controller.busy && code.isNotBlank()) Color(0xFF32ADE6) else Color(0xFF32ADE6).copy(alpha = 0.35f))
                .clickable(enabled = !controller.busy && code.isNotBlank()) { controller.pair(code) }
                .padding(vertical = 12.dp),
            contentAlignment = Alignment.Center,
        ) {
            if (controller.busy) {
                CircularProgressIndicator(color = Color.White, strokeWidth = 2.dp, modifier = Modifier.size(18.dp))
            } else {
                Text("连接", color = Color.White, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            }
        }
        controller.error?.let { Text(it, color = Color(0xFFE85D5D), fontSize = 13.sp) }
    }
    if (!cameraGranted) {
        LaunchedEffect(Unit) { permission.launch(Manifest.permission.CAMERA) }
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

private fun sendTerminalKey(controller: Controller, name: String, ctrl: Boolean) {
    val data = if (!ctrl) {
        TerminalInput.key(name)
    } else {
        when (name) {
            "arrowUp" -> "\u001b[1;5A"
            "arrowDown" -> "\u001b[1;5B"
            "arrowRight" -> "\u001b[1;5C"
            "arrowLeft" -> "\u001b[1;5D"
            "tab" -> "\u001b[27;5;9~"
            "escape" -> "\u001b[27;5;27~"
            else -> TerminalInput.key(name)
        }
    }
    if (data.isNotEmpty()) controller.sendRaw(data)
}

@Composable
private fun TerminalKey(label: String, textSize: Int = 14, armed: Boolean = false, onClick: () -> Unit) {
    val shape = if (label.length <= 1) CircleShape else RoundedCornerShape(percent = 50)
    Box(
        Modifier
            .height(36.dp)
            .widthIn(min = 36.dp)
            .clip(shape)
            .background(if (armed) Color(0xFF32ADE6) else Color.White.copy(alpha = 0.10f))
            .clickable(onClick = onClick)
            .padding(horizontal = if (label.length > 1) 10.dp else 0.dp),
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
        Modifier.fillMaxWidth().heightIn(min = 30.dp).clickable(onClick = onToggle).padding(start = 6.dp, end = 9.dp, top = 8.dp, bottom = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SidebarChevron(open, tint, 12.dp)
        Spacer(Modifier.width(6.dp))
        Text(name.uppercase(), color = Foreground, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (attention) {
            Spacer(Modifier.width(6.dp))
            Box(Modifier.size(6.dp).clip(CircleShape).background(Attention))
        }
        Spacer(Modifier.weight(1f))
        if (!open && count > 0) {
            Box(
                Modifier.clip(RoundedCornerShape(percent = 50)).background(Hover).height(18.dp).padding(horizontal = 6.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(count.toString(), color = Muted, fontSize = 11.sp)
            }
        }
    }
}

@Composable
private fun SidebarFeedbackFooter() {
    val context = LocalContext.current
    Row(
        Modifier
            .fillMaxWidth()
            .navigationBarsPadding()
            .drawWithContent {
                drawContent()
                drawLine(
                    Color.White.copy(alpha = 0.08f),
                    start = Offset.Zero,
                    end = Offset(size.width, 0f),
                    strokeWidth = 1.dp.toPx(),
                )
            }
            .clickable {
                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://github.com/orgs/unpeel-com/discussions")))
            }
            .padding(start = 14.dp, end = 14.dp, top = 12.dp, bottom = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FeedbackMark(Muted)
        Spacer(Modifier.width(8.dp))
        Text("反馈与问题", color = Muted, fontSize = 13.sp, fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
        ExternalArrow(Muted.copy(alpha = 0.7f))
    }
}

@Composable
private fun SidebarDisconnected(connection: String) {
    val unpaired = connection == "未配对"
    val connecting = connection == "正在连接"
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 22.dp, vertical = 36.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            when {
                unpaired -> "未配对"
                connecting -> "正在连接…"
                else -> "连接中断"
            },
            color = Foreground,
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.height(10.dp))
        Text(
            when {
                unpaired -> "先配对一台 Mac，会话才会出现在这里。"
                connecting -> "正在连接这台 Mac。"
                else -> "正在自动重连。Mac 恢复后，会话会重新出现。"
            },
            color = Muted,
            fontSize = 12.sp,
            textAlign = TextAlign.Center,
        )
        if (!unpaired) {
            Spacer(Modifier.height(14.dp))
            CircularProgressIndicator(color = Muted, modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        }
    }
}

private fun Modifier.sidebarListFade(): Modifier = drawWithContent {
    drawContent()
    drawRect(
        brush = Brush.verticalGradient(
            0f to Color.Transparent,
            0.06f to Color.Black,
            0.88f to Color.Black,
            1f to Color.Transparent,
        ),
        blendMode = BlendMode.DstIn,
    )
}

private fun selectedRowTint(hue: Double?): Color {
    if (hue == null || hue.isNaN()) return ActiveRow
    val wrapped = ((hue % 360.0) + 360.0) % 360.0
    return Color.hsv(wrapped.toFloat(), 0.55f, 0.52f, 0.30f)
}

@Composable
private fun WorkspaceTintDot(hue: Double?) {
    val fill = if (hue == null || hue.isNaN()) {
        Muted.copy(alpha = 0.4f)
    } else {
        val wrapped = ((hue % 360.0) + 360.0) % 360.0
        Color.hsv(wrapped.toFloat(), 0.62f, 0.85f)
    }
    Box(
        Modifier
            .size(9.dp)
            .clip(CircleShape)
            .background(fill)
            .border(0.5.dp, Color.White.copy(alpha = 0.18f), CircleShape),
    )
}

@Composable
private fun SidebarChevron(open: Boolean, tint: Color, size: Dp) {
    val degrees by animateFloatAsState(if (open) 90f else 0f, tween(150), label = "chevron")
    Canvas(Modifier.size(size).rotate(degrees)) {
        val stroke = Stroke(width = this.size.minDimension * 0.14f, cap = StrokeCap.Round, join = StrokeJoin.Round)
        val w = this.size.width
        val h = this.size.height
        drawPath(
            Path().apply {
                moveTo(w * 0.34f, h * 0.24f)
                lineTo(w * 0.66f, h * 0.50f)
                lineTo(w * 0.34f, h * 0.76f)
            },
            tint,
            style = stroke,
        )
    }
}

@Composable
private fun ChevronUpDown(tint: Color) {
    Canvas(Modifier.size(width = 10.dp, height = 12.dp)) {
        val stroke = Stroke(width = 1.4.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round)
        fun chevron(y0: Float, y1: Float, y2: Float) {
            drawPath(
                Path().apply {
                    moveTo(size.width * 0.15f, y0)
                    lineTo(size.width * 0.5f, y1)
                    lineTo(size.width * 0.85f, y2)
                },
                tint,
                style = stroke,
            )
        }
        chevron(size.height * 0.42f, size.height * 0.12f, size.height * 0.42f)
        chevron(size.height * 0.58f, size.height * 0.88f, size.height * 0.58f)
    }
}

@Composable
private fun WifiSlash(box: Dp, tint: Color) {
    Canvas(Modifier.size(box)) {
        val stroke = Stroke(width = 1.4.dp.toPx(), cap = StrokeCap.Round)
        drawLine(tint, Offset(size.width * 0.18f, size.height * 0.78f), Offset(size.width * 0.82f, size.height * 0.22f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawCircle(tint, radius = 1.3.dp.toPx(), center = Offset(size.width * 0.5f, size.height * 0.72f))
        drawArc(
            tint,
            startAngle = 200f,
            sweepAngle = 140f,
            useCenter = false,
            topLeft = Offset(size.width * 0.28f, size.height * 0.40f),
            size = androidx.compose.ui.geometry.Size(size.width * 0.44f, size.height * 0.36f),
            style = stroke,
        )
    }
}

@Composable
private fun ServerMark(box: Dp, tint: Color) {
    Canvas(Modifier.size(box)) {
        val stroke = Stroke(width = 1.dp.toPx(), cap = StrokeCap.Round)
        val inset = size.width * 0.12f
        drawRoundRect(
            tint,
            topLeft = Offset(inset, size.height * 0.08f),
            size = androidx.compose.ui.geometry.Size(size.width - inset * 2, size.height * 0.84f),
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(1.5.dp.toPx()),
            style = stroke,
        )
        drawLine(tint, Offset(inset, size.height * 0.40f), Offset(size.width - inset, size.height * 0.40f), strokeWidth = stroke.width)
        drawLine(tint, Offset(inset, size.height * 0.64f), Offset(size.width - inset, size.height * 0.64f), strokeWidth = stroke.width)
    }
}

@Composable
private fun FeedbackMark(tint: Color) {
    Canvas(Modifier.size(13.dp)) {
        val stroke = Stroke(width = 1.3.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round)
        val w = size.width
        val h = size.height
        drawRoundRect(
            tint,
            topLeft = Offset(w * 0.06f, h * 0.06f),
            size = androidx.compose.ui.geometry.Size(w * 0.88f, h * 0.68f),
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(2.dp.toPx()),
            style = stroke,
        )
        drawLine(tint, Offset(w * 0.22f, h * 0.74f), Offset(w * 0.18f, h * 0.94f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawLine(tint, Offset(w * 0.18f, h * 0.94f), Offset(w * 0.40f, h * 0.74f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawCircle(tint, radius = 0.8.dp.toPx(), center = Offset(w * 0.5f, h * 0.40f))
    }
}

@Composable
private fun ExternalArrow(tint: Color) {
    Canvas(Modifier.size(11.dp)) {
        val stroke = Stroke(width = 1.4.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round)
        val w = size.width
        val h = size.height
        drawLine(tint, Offset(w * 0.22f, h * 0.78f), Offset(w * 0.78f, h * 0.22f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawLine(tint, Offset(w * 0.42f, h * 0.22f), Offset(w * 0.78f, h * 0.22f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawLine(tint, Offset(w * 0.78f, h * 0.22f), Offset(w * 0.78f, h * 0.58f), strokeWidth = stroke.width, cap = StrokeCap.Round)
    }
}

private fun hostChrome(hue: Double?): Color {
    if (hue == null || hue.isNaN()) return Frame
    val wrapped = ((hue % 360.0) + 360.0) % 360.0
    return Color.hsl(wrapped.toFloat(), 0.18f, 0.10f)
}

private fun canPasteArtifact(kind: String, name: String): Boolean {
    if (kind == "screenshots" || kind == "computer" || kind == "uploads" || kind == "downloads") return true
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
