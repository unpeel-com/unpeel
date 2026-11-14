@file:OptIn(
    androidx.compose.foundation.ExperimentalFoundationApi::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package com.unpeel.android

import android.Manifest
import android.app.Activity
import android.app.Application
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.net.Uri
import android.view.inputmethod.InputMethodManager
import androidx.activity.BackEventCompat
import androidx.activity.compose.BackHandler
import androidx.activity.compose.PredictiveBackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.CubicBezierEasing
import androidx.compose.animation.core.LinearEasing
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
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.util.VelocityTracker
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
import androidx.compose.foundation.layout.imeAnimationTarget
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.SideEffect
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
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
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
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.TextUnit
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import com.termit.ui.AnsiColors
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

/**
 * SwiftUI `.cyan` has no hex in `Theme.swift` or the iOS sources.
 * This stand-in stays only on the call sites that use `.tint(.cyan)`.
 */
private val Cyan = Color(0xFF32ADE6)
/**
 * iOS session chrome + terminal card (`SessionDetailLayout` / `TerminalChrome`
 * / Ghostty window padding).
 */
private object SessionDetailLayout {
    val topChromePortrait = 78.dp
    val topChromeLandscape = 72.dp
    /** iOS `terminalBottomInset` — frame peeks under the card. */
    val bottomInset = 16.dp
    val cardCorner = 18.dp
    val cardPadH = 10.dp
    val cardPadV = 6.dp
    /** Key row above the IME: 4dp top + 36dp keys + 6dp bottom. */
    val keyboardAccessory = 46.dp
}

/** Classic CLI braille spinner as bitmasks (dots 1–8 → bits 0–7). No glyphs. */
private val SpinnerDotMasks = intArrayOf(
    0b00001011, // ⠋ 1 2 4
    0b00011001, // ⠙ 1 4 5
    0b00111001, // ⠹ 1 4 5 6
    0b00111000, // ⠸ 4 5 6
    0b00111100, // ⠼ 3 4 5 6
    0b00110100, // ⠴ 3 5 6
    0b00110110, // ⠦ 2 3 5 6
    0b00100110, // ⠧ 2 3 6
    0b00100111, // ⠇ 1 2 3 6
    0b00000111, // ⠏ 1 2 3
)

private enum class HomeSheet { Workspaces, Activity, Gallery, Organize, Presets, Project }

@Composable
fun UnpeelApp(controller: Controller) {
    val context = LocalContext.current
    val activity = context as? Activity
    val lock = remember { AppLock(context) }
    val credential = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        lock.finishCredential(result.resultCode == Activity.RESULT_OK)
    }
    // iOS locks on scenePhase .background only. .inactive (Face ID, a system
    // picker) does not lock and there is no timed grace. Count started
    // activities so a picker or the biometric sheet covering this activity
    // is not "left the app".
    DisposableEffect(activity) {
        val host = activity ?: return@DisposableEffect onDispose {}
        var started = 1
        val callbacks = object : Application.ActivityLifecycleCallbacks {
            override fun onActivityStarted(startedActivity: Activity) {
                val fromBackground = started == 0
                started += 1
                if (fromBackground) {
                    lock.onForeground(startedActivity)?.let { credential.launch(it) }
                }
            }

            override fun onActivityStopped(stoppedActivity: Activity) {
                started = (started - 1).coerceAtLeast(0)
                if (started == 0) lock.onBackground()
            }

            override fun onActivityCreated(a: Activity, b: android.os.Bundle?) {}
            override fun onActivityResumed(a: Activity) {}
            override fun onActivityPaused(a: Activity) {}
            override fun onActivitySaveInstanceState(a: Activity, b: android.os.Bundle) {}
            override fun onActivityDestroyed(a: Activity) {}
        }
        host.application.registerActivityLifecycleCallbacks(callbacks)
        lock.onForeground(host)?.let { credential.launch(it) }
        onDispose { host.application.unregisterActivityLifecycleCallbacks(callbacks) }
    }
    val settings = remember { PhoneSettings(context) }
    CompositionLocalProvider(LocalPhoneSettings provides settings) {
    val night = phoneNight()
    val chrome = hostChrome(controller.snapshot?.hostTintHue)
    val view = LocalView.current
    SideEffect {
        val window = (view.context as? Activity)?.window ?: return@SideEffect
        WindowCompat.getInsetsController(window, view).apply {
            isAppearanceLightStatusBars = !night
            isAppearanceLightNavigationBars = !night
        }
    }
    // The session screen is the root. A system back swipe must not finish the
    // activity. The terminal screen replaces that gesture with the drawer.
    BackHandler { }
    val scheme = if (night) {
        darkColorScheme(
            background = chrome,
            surface = Sheet,
            primary = Foreground,
            onPrimary = Color(0xFF121314),
            onBackground = Foreground,
            onSurface = Foreground,
            onSurfaceVariant = Muted,
        )
    } else {
        lightColorScheme(
            background = chrome,
            surface = Sheet,
            primary = Foreground,
            onPrimary = Color.White,
            onBackground = Foreground,
            onSurface = Foreground,
            onSurfaceVariant = Muted,
        )
    }
    MaterialTheme(colorScheme = scheme) {
        // Status bars are consumed inside TerminalHome so the sidebar can
        // paint under the notch like iOS (`ignoresSafeArea` + header inset).
        Box(Modifier.fillMaxSize().background(chrome)) {
            if (controller.selectedHost == null) {
                Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.statusBars)) {
                    PairScreen(
                        controller,
                        firstRun = true,
                        lock = lock,
                        onCredential = { credential.launch(it) },
                    )
                }
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
    var drawerPosition by remember { mutableFloatStateOf(0f) }
    var drawerDragging by remember { mutableStateOf(false) }
    var drawerOpen by remember { mutableStateOf(false) }
    var terminalSelecting by remember { mutableStateOf(false) }
    val session = controller.snapshot?.sessions?.firstOrNull { it.id == controller.openSessionID }
    LaunchedEffect(controller.openSessionID) {
        terminalSelecting = false
        if (controller.openSessionID != null) {
            drawerDragging = false
            drawerOpen = false
        }
    }
    Box(Modifier.fillMaxSize().background(hostChrome(controller.snapshot?.hostTintHue))) {
    BoxWithConstraints(Modifier.fillMaxSize()) {
        // iOS phone: min(360, max(286, w*0.88)); regular: min(380, max(320, w*0.40)).
        val drawerWidth = if (maxWidth >= 600.dp) {
            minOf(380.dp, maxOf(320.dp, maxWidth * 0.40f))
        } else {
            minOf(360.dp, maxOf(286.dp, maxWidth * 0.88f))
        }
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
            // iOS timingCurve(0.16, 1, 0.3, 1, duration: 0.28).
            animate(
                drawerPosition,
                target,
                animationSpec = tween(280, easing = CubicBezierEasing(0.16f, 1f, 0.3f, 1f)),
            ) { value, _ -> drawerPosition = value }
        }
        fun closeBy(totalX: Float) {
            drawerDragging = true
            drawerPosition = (drawerPx + totalX).coerceIn(0f, drawerPx)
        }
        fun releaseClose(dismiss: Boolean) {
            drawerDragging = false
            drawerOpen = !dismiss
        }
        Box(Modifier.fillMaxSize()) {
            // Terminal / empty state stay below the status bar; drawer does not.
            Box(
                Modifier
                    .fillMaxSize()
                    .windowInsetsPadding(WindowInsets.statusBars)
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
                    EmptyTerminal(
                        hostName = controller.selectedHost?.macName
                            ?: controller.snapshot?.macName,
                        paired = controller.selectedHost != null,
                        connecting = controller.snapshot == null && !controller.isUnreachable,
                        unreachable = controller.isUnreachable || controller.selectedHost == null,
                        onOpenSessions = { drawerOpen = true },
                        onPair = { sheet = HomeSheet.Workspaces },
                    )
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
        HomeSheet.Gallery -> GallerySheet(controller) { sheet = null }
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
                }) { Text(t("Remove"), color = Danger) }
            },
            dismissButton = { TextButton(onClick = { confirmingRemove = false }) { Text(t("Cancel")) } },
            title = { Text(t("Remove this session")) },
            text = { Text(t("Running processes will stop first. The session leaves the list; the conversation itself stays.")) },
            containerColor = Sheet,
        )
    }
    }
}

/**
 * iOS `EmptyTerminalState`: pixel mascot + calm Connecting / unreachable
 * escape hatch / Open Sessions glass CTA.
 */
@Composable
private fun EmptyTerminal(
    hostName: String?,
    paired: Boolean,
    connecting: Boolean,
    unreachable: Boolean,
    onOpenSessions: () -> Unit,
    onPair: () -> Unit,
) {
    Column(
        Modifier.fillMaxSize().padding(28.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        PixelMascot(76.dp)
        when {
            unreachable -> {
                Text(
                    hostName?.let { t("Can't reach %s").replace("%s", it) } ?: t("Not connected to a Mac"),
                    color = Foreground.copy(alpha = 0.60f),
                    fontSize = 15.sp,
                )
                EmptyGlassButton(
                    label = if (!paired) t("Pair with your Mac") else t("Connection…"),
                    tint = Cyan,
                    onClick = onPair,
                )
                if (paired) {
                    Text(
                        t("Make sure this phone and your Mac are on the same network, or that Unpeel Link is available."),
                        color = Foreground.copy(alpha = 0.50f),
                        fontSize = 13.sp,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.padding(horizontal = 24.dp),
                    )
                }
            }
            connecting -> {
                Text(
                    hostName?.let { t("Connecting to %s…").replace("%s", it) } ?: t("Connecting…"),
                    color = Foreground.copy(alpha = 0.60f),
                    fontSize = 15.sp,
                )
            }
            else -> {
                EmptyGlassButton(
                    label = t("Open Sessions"),
                    tint = null,
                    onClick = onOpenSessions,
                    leading = {
                        if (!SvgIcon("sidebar-toggle", 16.dp, tint = Foreground)) {
                            Text(t("☰"), color = Foreground, fontSize = 14.sp)
                        }
                    },
                )
            }
        }
    }
}

@Composable
private fun EmptyGlassButton(
    label: String,
    tint: Color?,
    onClick: () -> Unit,
    leading: (@Composable () -> Unit)? = null,
) {
    val fill = if (tint != null) {
        Brush.verticalGradient(listOf(tint, tint.copy(alpha = 0.85f)))
    } else {
        Brush.verticalGradient(
            listOf(Foreground.copy(alpha = 0.22f), Foreground.copy(alpha = 0.10f)),
        )
    }
    Row(
        Modifier
            .clip(RoundedCornerShape(percent = 50))
            .background(fill)
            .border(1.dp, Foreground.copy(alpha = 0.12f), RoundedCornerShape(percent = 50))
            .clickable(onClick = onClick)
            .padding(horizontal = 18.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        leading?.invoke()
        Text(
            label,
            color = if (tint != null) Color.White else Foreground,
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

/** iOS `PixelMascotView` — bundled webp frames at 200ms, nearest-neighbor. */
@Composable
private fun PixelMascot(size: Dp) {
    val context = LocalContext.current
    val frames = remember(context) {
        listOf(
            R.drawable.mascot_frame_0,
            R.drawable.mascot_frame_1,
            R.drawable.mascot_frame_2,
            R.drawable.mascot_frame_3,
        ).mapNotNull { id ->
            runCatching {
                android.graphics.BitmapFactory.decodeResource(context.resources, id)?.asImageBitmap()
            }.getOrNull()
        }
    }
    if (frames.size == 4) {
        val transition = rememberInfiniteTransition(label = "mascot")
        val phase by transition.animateFloat(
            initialValue = 0f,
            targetValue = 4f,
            animationSpec = infiniteRepeatable(
                animation = tween(durationMillis = 800, easing = LinearEasing),
                repeatMode = RepeatMode.Restart,
            ),
            label = "mascotPhase",
        )
        val frame = phase.toInt().floorMod(4)
        Image(
            bitmap = frames[frame],
            contentDescription = null,
            modifier = Modifier.size(size),
            filterQuality = FilterQuality.None,
        )
    } else {
        Box(
            Modifier.size(size).clip(RoundedCornerShape(22.dp)).background(Color(0xFF2A2118)),
            contentAlignment = Alignment.Center,
        ) {
            Text(t("U"), color = Color(0xFFF2A05A), fontSize = 32.sp, fontWeight = FontWeight.SemiBold)
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
    // False after background/foreground until the user taps the terminal.
    // A stuck IME inset must not keep shortening the card.
    var allowKeyboardInset by remember { mutableStateOf(true) }
    var terminalFocused by remember { mutableStateOf(false) }
    var ownsKeyboardInset by remember { mutableStateOf(false) }
    // Compose may revoke focus until this is true. Set from the tap, before
    // the posted showSoftInput.
    var keyboardWanted by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val pickPhoto = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val jpeg = withContext(Dispatchers.IO) { photoAttachJpeg(context.contentResolver, uri) }
            if (jpeg == null) controller.reportError("Couldn't read that photo")
            else controller.attachImage(jpeg)
        }
    }
    val lifecycleOwner = LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner, session.id) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME || event == Lifecycle.Event.ON_STOP) {
                allowKeyboardInset = false
                ownsKeyboardInset = false
                keyboardWanted = false
                terminalFocused = false
                terminalView?.dismissKeyboard()
                terminalView?.invalidate()
                terminalView?.rootView?.requestApplyInsets()
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            terminalView?.dismissKeyboard()
        }
    }
    DisposableEffect(session.id) { onDispose { controller.releasePhoneFit() } }
    fun dismissKeyboard() {
        keyboardWanted = false
        terminalView?.dismissKeyboard()
    }
    val frame = hostChrome(controller.snapshot?.hostTintHue)
    // iOS TerminalDetailView: full-bleed darker frame; lighter Surface only
    // inside the rounded terminal card. Top bar floats on the frame.
    BoxWithConstraints(Modifier.fillMaxSize().background(frame)) {
        val landscape = maxWidth > maxHeight
        val topBand = if (landscape) {
            SessionDetailLayout.topChromeLandscape
        } else {
            SessionDetailLayout.topChromePortrait
        }
        val density = LocalDensity.current
        val imeTargetBottom = WindowInsets.imeAnimationTarget.getBottom(density)
        val imeBottom = WindowInsets.ime.getBottom(density)
        // A hide that never reaches inset 0 (keyboard already gone, window still
        // reporting its height) would keep the blank band. Drop it once the
        // terminal is unfocused and the inset has stopped moving.
        LaunchedEffect(terminalFocused, imeBottom, imeTargetBottom, allowKeyboardInset) {
            // Unfocused, keyboard already gone, target still reporting the
            // old height. A show keeps focus, so this does not cut the slide.
            if (!allowKeyboardInset || terminalFocused || imeBottom > 0 || imeTargetBottom == 0) {
                return@LaunchedEffect
            }
            delay(500)
            if (!terminalFocused && imeBottom == 0) {
                allowKeyboardInset = false
                ownsKeyboardInset = false
            }
        }
        val keyboard = resolveKeyboardInset(
            allow = allowKeyboardInset,
            ownsInset = ownsKeyboardInset,
            terminalFocused = terminalFocused,
            imeTargetPx = imeTargetBottom,
            imePx = imeBottom,
        )
        var keyboardSpanPx by remember { mutableIntStateOf(0) }
        SideEffect {
            if (ownsKeyboardInset != keyboard.ownsInset) ownsKeyboardInset = keyboard.ownsInset
            val span = maxOf(imeTargetBottom, imeBottom)
            if (!keyboard.open && imeTargetBottom <= 0) {
                if (keyboardSpanPx != 0) keyboardSpanPx = 0
            } else if (span > keyboardSpanPx) {
                keyboardSpanPx = span
            }
        }
        val keyboardOpen = keyboard.open
        // IME inset already reaches the bottom of the window. Adding the
        // navigation bar on top of it left a black band and pushed the key
        // row up over the terminal. The key row grows with the same fraction
        // as the keyboard so it does not pop in on the first frame.
        val keyboardPad = with(density) { keyboard.padPx.toDp() }
        val navBottom = WindowInsets.navigationBars.getBottom(density)
        val accessoryFraction = if (keyboard.padPx > 0 && keyboardSpanPx > 0) {
            (keyboard.padPx.toFloat() / keyboardSpanPx).coerceIn(0f, 1f)
        } else {
            0f
        }
        val accessory = SessionDetailLayout.keyboardAccessory * accessoryFraction
        // The card keeps its resting height while the keyboard moves. A new
        // height would rasterize the grid on every frame. The picture slides
        // up instead; rows that pass under the title or the keyboard are
        // simply covered.
        val restingBottom = SessionDetailLayout.bottomInset + with(density) { navBottom.toDp() }
        val pictureShift = (keyboardPad + accessory - restingBottom - SessionDetailLayout.cardPadV)
            .coerceAtLeast(0.dp)
        // Lighter terminal card inset into the darker frame (iOS cardFrame).
        // Top band stays fixed so the title buttons stay above the card.
        TerminalGrid(
            controller,
            onView = { view ->
                terminalView = view
                view.onKeyboardRequested = {
                    keyboardWanted = true
                    allowKeyboardInset = true
                }
                view.onKeyboardFocusChanged = { focused ->
                    terminalFocused = focused
                    // A restored focus after background is not a tap. Only a
                    // request from showKeyboard may shorten the card again.
                    if (focused && keyboardWanted) allowKeyboardInset = true
                }
            },
            keyboardWanted = keyboardWanted,
            controlArmed = { ctrlArmed },
            onControlUsed = { ctrlArmed = false },
            onSelectionChanged = onSelectionChanged,
            keyboardOpen = keyboardOpen,
            showExitedRestart = session.status == "exited" && session.capabilities?.restart == true,
            restarting = controller.restartingSessionID == session.id,
            restartError = controller.restartError,
            onRestart = { controller.sessionAction(session.id, "restart") },
            pictureShift = pictureShift,
            modifier = Modifier
                .fillMaxSize()
                .padding(top = topBand, bottom = restingBottom),
        )
        // Soft dissolve under the title (iOS TerminalTopGlassVeil). Page chrome
        // is already HeaderInk; this only fades into the card.
        Box(
            Modifier
                .align(Alignment.TopCenter)
                .fillMaxWidth()
                .height(topBand + 8.dp)
                .background(
                    Brush.verticalGradient(
                        colorStops = arrayOf(
                            0.00f to frame.copy(alpha = 0.97f),
                            0.55f to frame.copy(alpha = 0.55f),
                            1.00f to Color.Transparent,
                        ),
                    ),
                ),
        )
        val approvalIDs = bellApprovalIDs(controller.snapshot?.pendingApprovals.orEmpty())
        val sessions = controller.snapshot?.sessions.orEmpty()
        val blocked = bellBlockedSessions(sessions, approvalIDs)
        val active = bellActiveSessions(sessions, blocked.map { it.id }.toSet())
        TerminalTopBar(
            session = session,
            project = project,
            galleryPulse = controller.galleryPulse,
            spinning = active.isNotEmpty(),
            hasBlockers = blocked.isNotEmpty(),
            spinnerColor = active.firstOrNull()?.let { toolSpinnerColor(it) } ?: GenericSpinner,
            onSessions = {
                dismissKeyboard()
                onSessions()
            },
            onOrganize = {
                dismissKeyboard()
                onSheet(HomeSheet.Organize)
            },
            onGallery = {
                dismissKeyboard()
                controller.loadArtifacts()
                onSheet(HomeSheet.Gallery)
            },
            onActivity = {
                dismissKeyboard()
                onSheet(HomeSheet.Activity)
            },
            modifier = Modifier
                .align(Alignment.TopCenter)
                .zIndex(2f)
                .fillMaxWidth()
                .height(topBand),
        )
        Column(
            Modifier
                .align(Alignment.BottomCenter)
                .zIndex(1f)
                .fillMaxWidth()
                .padding(bottom = keyboardPad),
        ) {
            controller.error?.let { message ->
                Row(Modifier.fillMaxWidth().padding(horizontal = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(t(message), color = Danger, fontSize = 12.sp, modifier = Modifier.weight(1f), maxLines = 2)
                    TextButton(onClick = controller::dismissError) { Text(t("Close")) }
                }
            }
            // iOS accessory bar: only while this terminal's keyboard is up.
            // The keys sit over the picture. Covered rows are not visible.
            if (accessoryFraction > 0f) {
                Row(
                    Modifier
                        .fillMaxWidth()
                        .height(accessory)
                        .clipToBounds(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Row(
                        Modifier
                            .weight(1f)
                            .horizontalScroll(rememberScrollState())
                            .padding(start = 12.dp, end = 4.dp, top = 4.dp, bottom = 6.dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        TerminalKey(t("Paste"), 12) {
                            if (terminalView?.pasteClipboard() != true) controller.reportError("Nothing to paste")
                        }
                        TerminalKey("◀") { sendTerminalKey(controller, "arrowLeft", ctrlArmed); ctrlArmed = false }
                        TerminalKey("▲") { sendTerminalKey(controller, "arrowUp", ctrlArmed); ctrlArmed = false }
                        TerminalKey("▼") { sendTerminalKey(controller, "arrowDown", ctrlArmed); ctrlArmed = false }
                        TerminalKey("▶") { sendTerminalKey(controller, "arrowRight", ctrlArmed); ctrlArmed = false }
                        TerminalKey("esc", 11) { sendTerminalKey(controller, "escape", ctrlArmed); ctrlArmed = false }
                        TerminalKey("Tab", 12) { sendTerminalKey(controller, "tab", ctrlArmed); ctrlArmed = false }
                        TerminalKey("Ctrl", 12, armed = ctrlArmed) { ctrlArmed = !ctrlArmed }
                    }
                    // iOS pins Attach Image at the trailing edge. No Dictate.
                    Box(Modifier.padding(end = 10.dp, top = 4.dp, bottom = 6.dp)) {
                        AttachImageKey {
                            pickPhoto.launch(
                                PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                            )
                        }
                    }
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

@Composable
private fun TerminalGrid(
    controller: Controller,
    onView: (HostTerminalView) -> Unit,
    keyboardWanted: Boolean = false,
    controlArmed: () -> Boolean,
    onControlUsed: () -> Unit,
    onSelectionChanged: (Boolean) -> Unit = {},
    keyboardOpen: Boolean = false,
    showExitedRestart: Boolean = false,
    restarting: Boolean = false,
    restartError: String? = null,
    onRestart: () -> Unit = {},
    pictureShift: Dp = 0.dp,
    bottomChromeInset: Dp = 0.dp,
    modifier: Modifier = Modifier,
) {
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
    // The phone's own light/dark, not the Host appearance. A provider hex
    // (Grok paints its own background) still fills the blank rows.
    val night = phoneNight()
    val mapGrok = AgentTerminalPolicy.rewritesGrokTruecolor(
        openSession?.providerID,
        openSession?.command.orEmpty(),
        openSession?.activeRuntimeID,
    )
    val mapOpenCode = AgentTerminalPolicy.rewritesOpenCodeCanvas(
        openSession?.providerID,
        openSession?.command.orEmpty(),
        openSession?.activeRuntimeID,
    )
    val canvas = terminalCanvas(openSession?.terminalBackgroundHex, night, mapGrok, mapOpenCode)
    val shownTerminal = controller.terminal
    val prefersRemoteWheel = AgentTerminalPolicy.prefersRemoteMouseWheel(
        openSession?.providerID,
        openSession?.command.orEmpty(),
        openSession?.activeRuntimeID,
    )
    val echoesMouseWheel = AgentTerminalPolicy.echoesMouseWheel(
        openSession?.providerID,
        openSession?.command.orEmpty(),
        openSession?.activeRuntimeID,
    )
    LaunchedEffect(controller.openSessionID) {
        surface?.clearSelection()
        selecting = false
        onSelectionChanged(false)
    }
    // Latch keyboard immediately from imeAnimationTarget — do not key this
    // effect on the interpolating ime inset (that restarted every frame).
    LaunchedEffect(controller.openSessionID, keyboardOpen) {
        if (keyboardOpen) {
            controller.notePhoneViewport(0, 0, keyboardOpen = true)
        }
    }
    // Last keyboard-up measurement. A resume can report this short height for
    // a frame after the inset drops; fitting it shrinks the PTY and the bottom
    // of the screen stays empty once the card grows back.
    var restingWidth by remember(controller.openSessionID) { mutableIntStateOf(0) }
    var restingHeight by remember(controller.openSessionID) { mutableIntStateOf(0) }
    // Chrome leaving/entering layout changes gridSize → re-fit Host PTY.
    LaunchedEffect(controller.openSessionID, gridSize, cellW, cellH, topPad, controller.connection, keyboardOpen) {
        if (keyboardOpen) return@LaunchedEffect
        if (gridSize.width < 48 || gridSize.height < 48 || cellW <= 0f || cellH <= 0f) return@LaunchedEffect
        val rotated = restingWidth > 0 && abs(gridSize.width - restingWidth) > 48
        val stillShort = !rotated && restingHeight > 0 && gridSize.height + 24 < restingHeight
        if (stillShort) {
            delay(400)
            if (keyboardOpen || gridSize.height + 24 >= restingHeight) return@LaunchedEffect
        } else {
            delay(120)
        }
        if (keyboardOpen) return@LaunchedEffect
        if (gridSize.width < 48 || gridSize.height < 48 || cellW <= 0f || cellH <= 0f) return@LaunchedEffect
        restingWidth = gridSize.width
        restingHeight = gridSize.height
        val grid = phoneFitGrid(gridSize.width, gridSize.height, cellW, cellH, topPad) ?: return@LaunchedEffect
        controller.notePhoneViewport(grid.first, grid.second, keyboardOpen = false)
    }
    val cardShape = RoundedCornerShape(SessionDetailLayout.cardCorner)
    val pictureShiftPx = with(LocalDensity.current) { pictureShift.toPx() }
    // Lighter Surface card on the darker frame behind (iOS Ghostty card).
    Box(modifier.clipToBounds()) {
        Box(
            Modifier
                .fillMaxSize()
                .shadow(
                    elevation = 2.dp,
                    shape = cardShape,
                    ambientColor = Color.Black.copy(alpha = 0.07f),
                    spotColor = Color.Black.copy(alpha = 0.07f),
                    clip = false,
                )
                .clip(cardShape)
                .background(Color(canvas))
                .border(1.dp, SheetHairline, cardShape)
                .padding(
                    horizontal = SessionDetailLayout.cardPadH,
                    vertical = SessionDetailLayout.cardPadV,
                ),
        ) {
            AndroidView(
                // Focusable only after a tap sets keyboardWanted. Leaving the
                // view focusable from the start pops the keyboard on open;
                // canFocus = false the whole time makes that tap's request
                // get revoked and the keyboard flash shut.
                modifier = Modifier
                    .fillMaxSize()
                    .onSizeChanged { gridSize = it }
                    .focusProperties { canFocus = keyboardWanted },
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
                    // Slide the already-drawn picture. translationY does not
                    // resize the view and does not record a new display list.
                    view.translationY = -pictureShiftPx
                    view.onSelectionChanged = {
                        selecting = it
                        onSelectionChanged(it)
                    }
                    // iOS shows the menu bar only while the keyboard is down.
                    // It does not take the keyboard away when a menu appears.
                    view.onMenuChanged = { active -> menuActive = active }
                    view.relayed = controller.usingRelay
                    view.prefersRemoteWheel = prefersRemoteWheel
                    view.echoesMouseWheel = echoesMouseWheel
                    // Keep in sync when activeRuntimeID arrives after open.
                    shownTerminal.holdLiveRepaints = echoesMouseWheel
                    shownTerminal.applySurface(night, canvas, mapGrok)
                    view.canvasColor = canvas
                    view.fitWidthToSharedGrid = controller.followingSharedFit
                    view.bind(shownTerminal)
                    view.onText = { controller.sendRaw(it) }
                    view.controlArmed = controlArmed
                    view.onControlUsed = onControlUsed
                    onView(view)
                },
            )
            if (selecting) {
                Row(
                    Modifier
                        .align(Alignment.TopCenter)
                        .zIndex(2f)
                        .padding(8.dp)
                        .background(Canvas.copy(alpha = 0xF0 / 255f), RoundedCornerShape(18.dp))
                        .border(1.dp, Foreground.copy(alpha = 0.12f), RoundedCornerShape(18.dp)),
                ) {
                    TextButton(onClick = {
                        if (surface?.copySelection() != true) controller.reportError("Nothing to copy")
                    }) { Text(t("Copy"), color = Foreground) }
                    TextButton(onClick = { surface?.clearSelection() }) { Text(t("Cancel"), color = Muted) }
                }
            }
            // Hide with the keyboard target latch — not the interpolating ime height.
            if (menuActive && !keyboardOpen) {
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
            // Another phone owns the letterbox — offer a one-tap claim. Owners hide this.
            if (controller.followingSharedFit && !selecting && !controller.sessionPreparing) {
                TextButton(
                    onClick = controller::claimPhoneFit,
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .zIndex(2f)
                        .padding(10.dp)
                        .background(Canvas.copy(alpha = 0xF0 / 255f), RoundedCornerShape(18.dp))
                        .border(1.dp, Foreground.copy(alpha = 0.12f), RoundedCornerShape(18.dp)),
                ) {
                    Text(t("Fit to this phone"), color = Foreground, fontSize = 13.sp)
                }
            }
            if (controller.sessionPreparing) {
                TerminalPrepareCover(canvas, Modifier.matchParentSize().zIndex(4f))
            }
            // iOS `ExitedSessionRestartBar`: floating capsule over the terminal.
            // Explicit animation.* receiver — ColumnScope.AnimatedVisibility would
            // otherwise win inside this ColumnScope composable.
            androidx.compose.animation.AnimatedVisibility(
                visible = showExitedRestart && !keyboardOpen && !selecting,
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .zIndex(3f)
                    .padding(bottom = bottomChromeInset + 12.dp),
                enter = expandVertically(tween(220), expandFrom = Alignment.Bottom) + fadeIn(tween(220)),
                exit = shrinkVertically(tween(180), shrinkTowards = Alignment.Bottom) + fadeOut(tween(160)),
            ) {
                ExitedRestartBar(
                    restarting = restarting,
                    errorText = restartError,
                    onRestart = onRestart,
                )
            }
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
        val ink = Color(AnsiColors.contrastingDefaultForeground(canvas))
        Column(Modifier.padding(start = 32.dp, end = 32.dp, top = 22.dp).graphicsLayer { this.alpha = alpha }) {
            Text(t("Preparing"), color = ink.copy(alpha = 0.72f), fontSize = 13.sp)
            Spacer(Modifier.height(18.dp))
            fractions.forEach { fraction ->
                Box(
                    Modifier
                        .padding(bottom = 13.dp)
                        .width(lineWidth * fraction)
                        .height(12.dp)
                        .background(ink.copy(alpha = 0.07f)),
                )
            }
        }
    }
}

/**
 * Terminal surface color. A per-session [hex] fills blank rows.
 * Grok rewrites it through GrokNight / GrokDay. OpenCode rewrites it
 * through its built-in canvas pairs. Every other agent keeps the hex,
 * or the phone's own light (`#ffffff`) / dark (`#1A1B1D`) canvas when
 * the Host sent none.
 */
private fun terminalCanvas(hex: Int?, night: Boolean, mapGrok: Boolean, mapOpenCode: Boolean): Int {
    if (hex != null) {
        val argb = 0xFF000000.toInt() or (hex and 0xFFFFFF)
        return when {
            mapGrok -> com.termit.ui.GrokTruecolor.map(argb, night)
            mapOpenCode -> com.termit.ui.OpenCodeCanvas.map(argb, night)
            else -> argb
        }
    }
    return if (night) AnsiColors.DARK_BACKGROUND else AnsiColors.LIGHT_BACKGROUND
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
    // Guard only the first tiny layout pass. Landscape / split-screen often
    // lands under 8 rows — returning null left the Host on the old portrait grid.
    if (columns < 20 || rows < 4) return null
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
    val tintHue = snapshot?.hostTintHue
    val night = phoneNight()
    // Mac surface over app background, Host-tint washed. Light is white on white.
    val sidebarBrush = Brush.verticalGradient(
        colors = listOf(
            Color(hostTintedArgb(MacChrome.surface(night) and 0xFFFFFF, tintHue)),
            Color(hostTintedArgb(MacChrome.appBackground(night) and 0xFFFFFF, tintHue)),
        ),
    )
    val drawerShape = RoundedCornerShape(topEnd = 16.dp, bottomEnd = 16.dp)
    Box(
        Modifier
            .fillMaxHeight()
            .width(width)
            // iOS: shadow(black@0.40, radius:22, x:8, y:0). Compose has no
            // x-offset — tint the elevation shadows to the same alpha.
            .shadow(
                elevation = 22.dp,
                shape = drawerShape,
                clip = false,
                ambientColor = Color.Black.copy(alpha = 0.40f),
                spotColor = Color.Black.copy(alpha = 0.40f),
            )
            .clip(drawerShape)
            .background(sidebarBrush)
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
            verticalArrangement = Arrangement.spacedBy(1.dp),
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
                .background(Hairline),
        )
    }
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
        ?: t("Workspace")
    val failing = snapshot == null && controller.connection != "Connecting" && controller.connection != "Unpaired"
    val subtitle = when {
        snapshot == null -> when (controller.connection) {
            "Unpaired" -> t("Unpaired")
            "Connecting" -> t("Connecting…")
            else -> t("Disconnected")
        }
        controller.usingRelay -> t("via Unpeel Remote")
        else -> null
    }
    val statusTop = WindowInsets.statusBars.asPaddingValues().calculateTopPadding()
    Row(
        Modifier
            .padding(start = 8.dp, end = 8.dp, top = statusTop + 14.dp)
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
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
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
            Spacer(Modifier.width(7.dp))
            SidebarCountBadge(snapshot.sessions.size)
        }
        Spacer(Modifier.width(7.dp))
        ChevronUpDown(Muted)
    }
}

/** iOS header/folder count capsule — 10pt medium in fixed 18pt height. */
@Composable
private fun SidebarCountBadge(count: Int, fontSize: TextUnit = 10.sp) {
    Box(
        Modifier
            .height(18.dp)
            .clip(RoundedCornerShape(percent = 50))
            .background(Hover)
            .padding(horizontal = 6.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            count.toString(),
            color = Muted,
            maxLines = 1,
            softWrap = false,
            style = TextStyle(
                color = Muted,
                fontSize = fontSize,
                fontWeight = FontWeight.Medium,
                platformStyle = PlatformTextStyle(includeFontPadding = false),
                lineHeight = 18.sp,
                lineHeightStyle = LineHeightStyle(
                    alignment = LineHeightStyle.Alignment.Center,
                    trim = LineHeightStyle.Trim.Both,
                ),
            ),
            modifier = Modifier.wrapContentHeight(align = Alignment.CenterVertically, unbounded = true),
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
    val tint = folderSwatch(project.colorID) ?: Muted
    val child = isInlineSidebarProject(project)
    val leading = if (child) (18 + (depth - 1).coerceAtLeast(0) * 14).dp else (7 + depth * 14).dp
    val folderAsset = if (open) "folder-open" else "folder-closed"
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
                SidebarChevron(open, tint, 11.dp)
            } else if (!SvgIcon(folderAsset, 18.dp, tint = tint)) {
                SidebarChevron(open, tint, 11.dp)
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
                if (!SvgIcon("pin", 11.dp, tint = Muted)) Text(t("📌"), fontSize = 11.sp)
            }
            // iOS: worktreeBranch always shows the icon; label only when ≠ name.
            val branch = project.worktreeBranch
            if (!branch.isNullOrEmpty()) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(start = 4.dp)) {
                    BranchMark(12.dp, Muted.copy(alpha = 0.55f))
                    if (branch != project.name) {
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
                if (!SvgIcon("plus", 17.dp, tint = Muted)) Text(t("+"), color = Muted, fontSize = 16.sp)
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
    // iOS: padding then selectedRowBackground — chip is full-width, content inset.
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 32.dp)
            .clip(RoundedCornerShape(9.dp))
            .background(if (selected) selectedRowTint(tintHue) else Color.Transparent)
            .combinedClickable(onClick = onOpen, onLongClick = onOrganize)
            .padding(
                start = (11 + (depth - 1).coerceAtLeast(0) * 14).dp,
                top = 2.dp,
                end = 9.dp,
                bottom = 2.dp,
            )
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
                    Box(Modifier.graphicsLayer { scaleX = 0.82f; scaleY = 0.82f }) {
                        BrailleSpinner(toolSpinnerColor(session))
                    }
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
                if (!SvgIcon("pin", 11.dp, tint = Muted)) Text(t("📌"), fontSize = 11.sp)
            }
        }
        Spacer(Modifier.width(4.dp))
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            if (session.notifyWhenDone) {
                if (!SvgIcon("bell", 11.dp, tint = Muted)) Text(t("🔔"), fontSize = 11.sp)
            }
            Text(
                if (blocked) "blocked" else relativeAge(session.updatedAtUnixMs),
                color = Muted,
                fontSize = 10.sp,
                fontWeight = if (selected) FontWeight.Medium else FontWeight.Normal,
                modifier = Modifier.widthIn(min = 24.dp),
                textAlign = TextAlign.End,
            )
            Box(
                Modifier
                    .padding(start = 1.dp)
                    .size(14.dp)
                    .graphicsLayer { alpha = 0.82f },
                contentAlignment = Alignment.Center,
            ) {
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
}

@Composable
private fun ApprovalOverlay(approval: PendingApproval, more: Int, onAnswer: (String, Boolean) -> Unit) {
    Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.28f)), contentAlignment = Alignment.Center) {
        Column(
            Modifier.widthIn(max = 340.dp).padding(horizontal = 28.dp).clip(RoundedCornerShape(22.dp)).background(Sheet).border(1.dp, SheetHairline, RoundedCornerShape(22.dp)).padding(22.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(t("?"), color = Color.Yellow, fontSize = 28.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(10.dp))
            Text(approval.title, color = Foreground, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            Text(approval.body, color = Muted, fontSize = 13.sp)
            if (more > 0) {
                Spacer(Modifier.height(6.dp))
                Text(t("%d more waiting").replace("%d", more.toString()), color = Muted.copy(alpha = 0.7f), fontSize = 11.sp)
            }
            Spacer(Modifier.height(14.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                TextButton(onClick = { onAnswer(approval.id, false) }, modifier = Modifier.weight(1f)) { Text(t("Don’t allow")) }
                Button(onClick = { onAnswer(approval.id, true) }, modifier = Modifier.weight(1f)) { Text(t("Allow")) }
            }
        }
    }
}

/**
 * iOS `PairingView` — pairing IS a workspace. One card lists paired hosts;
 * tap switches connection, minus forgets (with confirm), footer adds another.
 * No cross-workspace RPC list (removed on iOS 2026-08-23).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HostSheet(
    controller: Controller,
    lock: AppLock,
    onCredential: (Intent) -> Unit,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    var addingMac by remember { mutableStateOf(false) }
    var forgetHost by remember { mutableStateOf<SavedHost?>(null) }
    val hosts = controller.hosts
    val empty = hosts.isEmpty()
    // iOS keeps ONE sheet and only grows the detent when adding
    // (`[.large]` vs `[.medium,.large]`). Do NOT key skipPartiallyExpanded on
    // addingMac — rememberSaveable would recreate SheetState and re-pop the
    // modal. Expand in place instead.
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = empty)
    LaunchedEffect(addingMac) {
        if (addingMac) sheetState.expand()
    }
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
        containerColor = Canvas,
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    if (empty) t("Pair with a workspace") else t("Workspaces"),
                    color = Foreground,
                    fontSize = 17.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onDismiss) {
                    Text(t("Done"), color = Cyan, fontSize = 16.sp, fontWeight = FontWeight.Medium)
                }
            }
            if (empty) {
                PairingBrandHeader()
                UnpairedGuidanceCard()
                PairFields(controller, context)
                PairingErrorLine(controller.error)
                PhonePreferenceCard()
                SecuritySettingsCard(lock, onCredential)
            } else {
                WorkspacesListCard(
                    hosts = hosts,
                    activeMacID = controller.selectedHost?.macID,
                    liveTintHue = controller.snapshot?.hostTintHue,
                    usingRelay = controller.usingRelay,
                    addingMac = addingMac,
                    onSelect = { macID ->
                        if (macID != controller.selectedHost?.macID) {
                            controller.select(macID)
                        }
                        onDismiss()
                    },
                    onForget = { forgetHost = it },
                    onAdd = { addingMac = true },
                )
                if (addingMac) {
                    Row(
                        Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            t("Pair the new workspace"),
                            color = Foreground.copy(alpha = 0.85f),
                            fontSize = 14.sp,
                            fontWeight = FontWeight.SemiBold,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = {
                            addingMac = false
                            controller.clearError()
                        }) {
                            Text(t("Cancel"), color = Cyan, fontSize = 14.sp)
                        }
                    }
                    PairFields(controller, context)
                    PairingErrorLine(controller.error)
                }
                PhonePreferenceCard()
                SecuritySettingsCard(lock, onCredential)
            }
            Spacer(Modifier.height(8.dp))
        }
    }
    // iOS: successful add dismisses the sheet onto the new workspace.
    var hostsAtAdd by remember { mutableIntStateOf(0) }
    LaunchedEffect(addingMac) {
        if (addingMac) hostsAtAdd = controller.hosts.size
    }
    LaunchedEffect(addingMac, controller.busy, controller.hosts.size) {
        if (addingMac && !controller.busy && controller.hosts.size > hostsAtAdd) {
            onDismiss()
        }
    }
    forgetHost?.let { host ->
        AlertDialog(
            onDismissRequest = { forgetHost = null },
            title = { Text(t("Forget %s?").replace("%s", host.macName)) },
            text = {
                Text(
                    t("This phone will no longer be able to control this workspace. You can also revoke this phone in the workspace's Settings on the Mac."),
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    controller.forget(host.macID)
                    forgetHost = null
                }) {
                    Text(t("Forget %s").replace("%s", host.macName), color = Danger)
                }
            },
            dismissButton = {
                TextButton(onClick = { forgetHost = null }) { Text(t("Cancel")) }
            },
            containerColor = Sheet,
        )
    }
}

@Composable
private fun PairingBrandHeader() {
    Column(
        Modifier.fillMaxWidth().padding(top = 8.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(
            Modifier.size(68.dp).clip(RoundedCornerShape(20.dp)).background(Color(0xFF2A2118)),
            contentAlignment = Alignment.Center,
        ) {
            Text(t("U"), color = Color(0xFFF2A05A), fontSize = 32.sp, fontWeight = FontWeight.SemiBold)
        }
        Text(t("Unpeel"), color = Foreground, fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
    }
}

@Composable
private fun UnpairedGuidanceCard() {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(SoftCard)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            WifiSlash(16.dp, Foreground.copy(alpha = 0.7f))
            Text(t("Not paired"), color = Foreground.copy(alpha = 0.7f), fontSize = 14.sp)
        }
        Text(
            t("Open Unpeel on your Mac, switch to the workspace you want, and show the pairing code in Settings ▸ iPhone. Each workspace pairs separately."),
            color = Foreground.copy(alpha = 0.5f),
            fontSize = 13.sp,
        )
    }
}

@Composable
private fun WorkspacesListCard(
    hosts: List<SavedHost>,
    activeMacID: String?,
    liveTintHue: Double?,
    usingRelay: Boolean,
    addingMac: Boolean,
    onSelect: (String) -> Unit,
    onForget: (SavedHost) -> Unit,
    onAdd: () -> Unit,
) {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(SoftCard)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        hosts.forEach { host ->
            val active = host.macID == activeMacID
            val tint = if (active) (liveTintHue ?: host.tintHue) else host.tintHue
            Row(
                Modifier.fillMaxWidth().padding(vertical = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Row(
                    Modifier
                        .weight(1f)
                        .heightIn(min = 44.dp)
                        .clickable { onSelect(host.macID) },
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    WorkspaceTintDot(tint, size = 12.dp)
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        Text(
                            host.macName,
                            color = Foreground,
                            fontSize = 14.sp,
                            fontWeight = if (active) FontWeight.SemiBold else FontWeight.Normal,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            if (active && usingRelay) "via Unpeel Remote" else host.endpoint,
                            color = Foreground.copy(alpha = 0.55f),
                            fontSize = 12.sp,
                            fontFamily = FontFamily.Monospace,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    if (active) {
                        CyanCheckmark()
                    }
                }
                Box(
                    Modifier
                        .size(36.dp)
                        .clickable { onForget(host) },
                    contentAlignment = Alignment.Center,
                ) {
                    MinusCircle(Foreground.copy(alpha = 0.45f))
                }
            }
        }
        if (!addingMac) {
            Row(
                Modifier
                    .padding(top = 4.dp)
                    .clickable(onClick = onAdd),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                PlusCircle(Cyan)
                Text(t("Add a Workspace"), color = Cyan, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
            }
        }
    }
}

@Composable
private fun PhonePreferenceCard() {
    val settings = LocalPhoneSettings.current ?: return
    val tracking = if (phoneSimplifiedChinese()) 0.sp else 0.6.sp
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(SoftCard)
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            t("THEME"),
            color = Foreground.copy(alpha = 0.5f),
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
            letterSpacing = tracking,
        )
        ThemeChoice.entries.forEach { choice ->
            PreferenceChoice(
                label = when (choice) {
                    ThemeChoice.System -> t("Follow system")
                    ThemeChoice.Dark -> t("Dark")
                    ThemeChoice.Light -> t("Light")
                },
                selected = settings.theme == choice,
                onClick = { settings.chooseTheme(choice) },
            )
        }
        Spacer(Modifier.height(8.dp))
        Text(
            t("LANGUAGE"),
            color = Foreground.copy(alpha = 0.5f),
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
            letterSpacing = tracking,
        )
        LanguageChoice.entries.forEach { choice ->
            PreferenceChoice(
                label = when (choice) {
                    LanguageChoice.System -> t("Follow system")
                    LanguageChoice.English -> t("English")
                    LanguageChoice.SimplifiedChinese -> "简体中文"
                },
                selected = settings.language == choice,
                onClick = { settings.chooseLanguage(choice) },
            )
        }
    }
}

@Composable
private fun PreferenceChoice(label: String, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 40.dp)
            .clickable(onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = Foreground, fontSize = 15.sp, modifier = Modifier.weight(1f))
        if (selected) Text(t("✓"), color = Accent, fontSize = 16.sp)
    }
}

@Composable
private fun SecuritySettingsCard(lock: AppLock, onCredential: (Intent) -> Unit) {
    val context = LocalContext.current
    val activity = context as? Activity
    val available = lock.capabilityAvailable()
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(SoftCard)
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        val tracking = if (phoneSimplifiedChinese()) 0.sp else 0.6.sp
        Text(
            t("SECURITY"),
            color = Foreground.copy(alpha = 0.5f),
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
            letterSpacing = tracking,
        )
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(t("Require ${lock.methodLabel()}"), color = Foreground, fontSize = 15.sp)
                Text(
                    t("Locks Unpeel when you leave the app."),
                    color = Foreground.copy(alpha = 0.5f),
                    fontSize = 12.sp,
                )
            }
            Switch(
                checked = lock.enabled,
                enabled = available,
                colors = SwitchDefaults.colors(
                    checkedTrackColor = Accent,
                    checkedThumbColor = Color.White,
                    checkedBorderColor = Accent,
                ),
                onCheckedChange = { armed ->
                    if (activity == null) return@Switch
                    if (armed) lock.arm(activity)?.let(onCredential) else lock.disarm()
                },
            )
        }
        if (!available) {
            Text(
                t("Set a device screen lock (and enroll biometrics) in system Settings to use the app lock."),
                color = Foreground.copy(alpha = 0.5f),
                fontSize = 12.sp,
            )
        }
        lock.lastError?.let {
            Text(t(it), color = Danger, fontSize = 12.sp)
        }
    }
}

@Composable
private fun CyanCheckmark() {
    Canvas(Modifier.size(22.dp)) {
        drawCircle(Cyan)
        val stroke = Stroke(width = 2.2.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round)
        val path = Path().apply {
            moveTo(size.width * 0.28f, size.height * 0.52f)
            lineTo(size.width * 0.44f, size.height * 0.68f)
            lineTo(size.width * 0.72f, size.height * 0.34f)
        }
        drawPath(path, Color.White, style = stroke)
    }
}

@Composable
private fun MinusCircle(tint: Color) {
    Canvas(Modifier.size(22.dp)) {
        val stroke = Stroke(width = 1.6.dp.toPx())
        drawCircle(tint, style = stroke)
        drawLine(
            tint,
            Offset(size.width * 0.28f, size.height * 0.5f),
            Offset(size.width * 0.72f, size.height * 0.5f),
            strokeWidth = 1.8.dp.toPx(),
            cap = StrokeCap.Round,
        )
    }
}

@Composable
private fun PlusCircle(tint: Color) {
    Canvas(Modifier.size(18.dp)) {
        val stroke = Stroke(width = 1.6.dp.toPx())
        drawCircle(tint, style = stroke)
        drawLine(
            tint,
            Offset(size.width * 0.5f, size.height * 0.28f),
            Offset(size.width * 0.5f, size.height * 0.72f),
            strokeWidth = 1.8.dp.toPx(),
            cap = StrokeCap.Round,
        )
        drawLine(
            tint,
            Offset(size.width * 0.28f, size.height * 0.5f),
            Offset(size.width * 0.72f, size.height * 0.5f),
            strokeWidth = 1.8.dp.toPx(),
            cap = StrokeCap.Round,
        )
    }
}

/**
 * iOS `ActivitySessionsPanel` — Blocked / Active / Recent buckets, same
 * type scale and row chrome as the phone Controller.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ActivitySheet(controller: Controller, onDismiss: () -> Unit, onSelect: (String) -> Unit) {
    val sessions = controller.snapshot?.sessions.orEmpty()
    val projects = controller.snapshot?.projects.orEmpty().associateBy { it.id }
    val openID = controller.openSessionID
    val approvalIDs = bellApprovalIDs(controller.snapshot?.pendingApprovals.orEmpty())
    val blocked = bellBlockedSessions(sessions, approvalIDs)
    val blockerIDs = blocked.map { it.id }.toSet()
    val active = bellActiveSessions(sessions, blockerIDs)
    val reservedIDs = sessions.mapNotNull { session ->
        if (session.status == "running" &&
            (session.activity == "blocked" || session.activity == "working" || session.id in approvalIDs)
        ) {
            session.id
        } else {
            null
        }
    }.toSet()
    val recent = sessions
        .filter { it.id != openID && it.id !in reservedIDs }
        .distinctBy { it.id }
        .sortedWith(
            compareByDescending<SessionSummary> { it.unread }
                .thenByDescending { it.updatedAtUnixMs ?: 0L },
        )
        .take(8)
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Panel,
    ) {
        Column(
            Modifier
                .padding(top = 16.dp, start = 12.dp, end = 12.dp, bottom = 20.dp)
                .verticalScroll(rememberScrollState()),
        ) {
            if (blocked.isEmpty() && active.isEmpty() && recent.isEmpty()) {
                Text(
                    t("No active sessions"),
                    color = Foreground.copy(alpha = 0.62f),
                    fontSize = 13.sp,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.padding(18.dp),
                )
            } else {
                if (blocked.isNotEmpty()) {
                    ActivityPanelSectionTitle(t("Blocked"))
                    blocked.forEach { session ->
                        ActivityPanelRow(
                            session = session,
                            projectName = projects[session.projectID]?.name,
                            working = false,
                            blocked = true,
                            onSelect = { onSelect(session.id) },
                        )
                    }
                }
                if (active.isNotEmpty()) {
                    if (blocked.isNotEmpty()) ActivityPanelDivider()
                    ActivityPanelSectionTitle(t("Active"))
                    active.forEach { session ->
                        ActivityPanelRow(
                            session = session,
                            projectName = projects[session.projectID]?.name,
                            working = true,
                            blocked = false,
                            onSelect = { onSelect(session.id) },
                        )
                    }
                }
                if (recent.isNotEmpty()) {
                    if (blocked.isNotEmpty() || active.isNotEmpty()) ActivityPanelDivider()
                    ActivityPanelSectionTitle(t("Recent"))
                    recent.forEach { session ->
                        ActivityPanelRow(
                            session = session,
                            projectName = projects[session.projectID]?.name,
                            working = false,
                            blocked = false,
                            onSelect = { onSelect(session.id) },
                        )
                    }
                }
            }
            Spacer(Modifier.height(12.dp))
        }
    }
}

@Composable
private fun ActivityPanelSectionTitle(title: String) {
    Text(
        title.uppercase(),
        color = Foreground.copy(alpha = 0.46f),
        fontSize = 11.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier.padding(start = 8.dp, top = 3.dp, end = 8.dp, bottom = 6.dp),
    )
}

@Composable
private fun ActivityPanelDivider() {
    Box(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .height(1.dp)
            .background(Hairline),
    )
}

@Composable
private fun ActivityPanelRow(
    session: SessionSummary,
    projectName: String?,
    working: Boolean,
    blocked: Boolean,
    onSelect: () -> Unit,
) {
    val context = projectName?.takeIf { it.isNotEmpty() }
        ?: session.presentationProviderID()
        ?: t("Terminal")
    val subtitle = listOfNotNull(context, session.worktreeBranch).joinToString("  ")
    val trailing = when {
        blocked -> t("Blocked")
        working -> t("run")
        else -> relativeAge(session.updatedAtUnixMs)
    }
    val trailingColor = when {
        blocked -> Attention
        else -> Foreground.copy(alpha = 0.42f)
    }
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onSelect)
            .padding(start = 8.dp, top = 7.dp, end = 8.dp, bottom = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
            when {
                working -> BrailleSpinner(toolSpinnerColor(session))
                blocked -> {
                    Box(Modifier.size(15.dp).clip(CircleShape).background(Attention.copy(alpha = 0.20f)))
                    Box(Modifier.size(7.dp).clip(CircleShape).background(Attention))
                }
                session.unread -> Box(Modifier.size(7.dp).clip(CircleShape).background(Unread))
            }
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(
                session.title,
                color = Foreground.copy(alpha = 0.90f),
                fontSize = 14.sp,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                subtitle,
                color = Foreground.copy(alpha = 0.48f),
                fontSize = 11.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.width(6.dp))
        if (trailing.isNotEmpty()) {
            Text(
                t(trailing),
                color = trailingColor,
                fontSize = 10.sp,
                fontWeight = FontWeight.Medium,
                maxLines = 1,
            )
            Spacer(Modifier.width(8.dp))
        }
        Box(Modifier.graphicsLayer { alpha = 0.74f }) {
            ToolLogo(
                session.command,
                session.providerID,
                session.activeRuntimeID,
                session.status == "running",
                16.dp,
            )
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
            Text(t("Edit session"), color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(10.dp))
            OutlinedTextField(value = title, onValueChange = { title = it }, singleLine = true, modifier = Modifier.fillMaxWidth())
            if (showNotify) {
                Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(t("Notify when done"), color = Foreground, fontSize = 15.sp)
                        Text(t("Alert you when this turn finishes"), color = Muted, fontSize = 12.sp)
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
                ) { Text(t("Save")) }
                TextButton(onClick = { controller.organize(session.id, pinned = !session.pinned) }) {
                    Text(if (session.pinned) "Unpin" else "Pin")
                }
            }
            if (showTranscript) {
                Text(t("Copy transcript"), color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp))
                Row {
                    listOf(20 to t("Last 20"), 50 to t("Last 50"), 0 to t("All")).forEach { (entries, label) ->
                        TextButton(onClick = {
                            controller.copyTranscript(session.id, entries) { markdown, failure ->
                                if (markdown != null) {
                                    val clipboard = context.getSystemService(android.content.ClipboardManager::class.java)
                                    clipboard?.setPrimaryClip(android.content.ClipData.newPlainText("transcript", markdown))
                                    note = "Copied as Markdown"
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
                }) { Text(t("Resume agent")) }
                ResumeOffer.Session -> TextButton(onClick = {
                    controller.sessionAction(session.id, "restart")
                    onDismiss()
                }) { Text(t("Resume")) }
                ResumeOffer.None -> Unit
            }
            if (canStopSession(session)) {
                TextButton(onClick = { confirmStop = true }) { Text(t("Stop")) }
            }
            TextButton(onClick = onRemove) { Text(if (session.status == "running") t("Remove session") else t("Remove from list"), color = Danger) }
            note?.let { Text(t(it), color = if (it.startsWith("Copied")) Muted else Danger, fontSize = 12.sp) }
            Text(t(organizeHint(session, offer)), color = Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 6.dp, bottom = 18.dp))
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
                }) { Text(t("Stop")) }
            },
            dismissButton = { TextButton(onClick = { confirmStop = false }) { Text(t("Cancel")) } },
            title = { Text(t("Stop this session")) },
            text = { Text(t("The process stops. The session stays in the list so you can resume later.")) },
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
    val saveEnabled = canEdit && (!canRename || name.isNotBlank())
    val barButtonColors = ButtonDefaults.textButtonColors(
        contentColor = Accent,
        disabledContentColor = Muted,
    )
    fun save() {
        if (!saveEnabled) return
        val trimmed = name.trim()
        val namePatch = trimmed.takeIf { canRename && it.isNotEmpty() && it != project.name }
        val colorPatch = when {
            !canColor || colorID == project.colorID -> null
            else -> colorID ?: ""
        }
        val sortPatch = dateSorted.takeIf { it != (project.dateSorted == true) }
        if (namePatch == null && colorPatch == null && sortPatch == null) {
            onDismiss()
            return
        }
        controller.saveProject(project.id, namePatch, colorPatch, sortPatch) { failure ->
            if (failure == null) onDismiss() else note = failure
        }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = HeaderInk) {
        Column(Modifier.padding(horizontal = 18.dp).verticalScroll(rememberScrollState())) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                TextButton(
                    onClick = onDismiss,
                    contentPadding = PaddingValues(horizontal = 0.dp, vertical = 4.dp),
                    colors = barButtonColors,
                ) { Text(t("Cancel"), fontSize = 17.sp) }
                Text(
                    if (project.isGroup) t("Edit group") else t("Edit folder"),
                    color = Foreground,
                    fontSize = 17.sp,
                    fontWeight = FontWeight.SemiBold,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.weight(1f),
                )
                if (canEdit) {
                    TextButton(
                        onClick = ::save,
                        enabled = saveEnabled,
                        contentPadding = PaddingValues(horizontal = 0.dp, vertical = 4.dp),
                        colors = barButtonColors,
                    ) { Text(t("Save"), fontSize = 17.sp, fontWeight = FontWeight.SemiBold) }
                }
            }
            Spacer(Modifier.height(10.dp))
            if (!canEdit) {
                Text(t("Update Unpeel on the Mac first to organize folders from the phone."), color = Muted, fontSize = 14.sp)
            }
            if (canRename) {
                Text(t("Folder name"), color = Muted, fontSize = 13.sp, modifier = Modifier.padding(top = 12.dp, bottom = 6.dp))
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    shape = RoundedCornerShape(10.dp),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedBorderColor = Color.Transparent,
                        unfocusedBorderColor = Color.Transparent,
                        focusedContainerColor = Canvas,
                        unfocusedContainerColor = Canvas,
                        cursorColor = Accent,
                        focusedTextColor = Foreground,
                        unfocusedTextColor = Foreground,
                    ),
                )
            }
            if (canEdit) {
                Text(t("Sort sessions"), color = Muted, fontSize = 13.sp, modifier = Modifier.padding(top = 14.dp, bottom = 6.dp))
                Column(
                    Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .background(Canvas),
                ) {
                    listOf(false to t("Custom order"), true to t("Date (newest first)")).forEach { (value, label) ->
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .clickable { dateSorted = value }
                                .padding(horizontal = 16.dp, vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(label, color = Foreground, fontSize = 17.sp, modifier = Modifier.weight(1f))
                            if (dateSorted == value) Text(t("✓"), color = Accent, fontSize = 17.sp)
                        }
                    }
                }
            }
            if (canColor) {
                Text(t("Folder color"), color = Muted, fontSize = 13.sp, modifier = Modifier.padding(top = 14.dp, bottom = 6.dp))
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(10.dp))
                        .background(Canvas)
                        .horizontalScroll(rememberScrollState())
                        .padding(horizontal = 12.dp, vertical = 12.dp),
                ) {
                    colors.forEach { id ->
                        val fill = folderSwatch(id)
                        val selected = colorID == id
                        Box(
                            Modifier
                                .size(28.dp)
                                .clip(CircleShape)
                                .background(fill ?: Color.Transparent)
                                .border(
                                    if (selected) 2.dp else 1.5.dp,
                                    when {
                                        selected -> Accent
                                        fill == null -> Muted
                                        else -> Color.Transparent
                                    },
                                    CircleShape,
                                )
                                .clickable { colorID = id },
                        )
                    }
                }
            }
            note?.let { Text(t(it), color = Danger, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp)) }
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
            Text(t("New session"), color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(horizontal = 8.dp))
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
            if (presets.isEmpty()) Text(t("This Host has no presets available."), color = Muted, modifier = Modifier.padding(12.dp))
            Spacer(Modifier.height(20.dp))
        }
    }
}

@Composable
private fun PairScreen(
    controller: Controller,
    firstRun: Boolean,
    lock: AppLock? = null,
    onCredential: ((Intent) -> Unit)? = null,
) {
    val context = LocalContext.current
    // iOS firstPairingContent — brand, unpaired guidance, scanner/paste, security.
    Column(
        Modifier
            .fillMaxSize()
            .background(Canvas)
            .navigationBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        PairingBrandHeader()
        UnpairedGuidanceCard()
        PairFields(controller, context)
        PairingErrorLine(controller.error)
        if (firstRun && lock != null && onCredential != null) {
            PhonePreferenceCard()
            SecuritySettingsCard(lock, onCredential)
        }
    }
}

/** iOS `errorSection` — sits outside scanner/paste, footnote medium red. */
@Composable
private fun PairingErrorLine(message: String?) {
    if (message.isNullOrBlank()) return
    Text(
        message,
        color = Danger.copy(alpha = 0.9f),
        fontSize = 13.sp,
        fontWeight = FontWeight.Medium,
        modifier = Modifier.fillMaxWidth(),
    )
}

/** Emulator / no-camera — mirror iOS `#if !targetEnvironment(simulator)`. */
private fun pairingScannerAvailable(context: android.content.Context): Boolean {
    val fingerprint = android.os.Build.FINGERPRINT
    val model = android.os.Build.MODEL
    val product = android.os.Build.PRODUCT
    val emulator = fingerprint.startsWith("generic") ||
        fingerprint.contains("emulator", ignoreCase = true) ||
        model.contains("Emulator", ignoreCase = true) ||
        model.contains("Android SDK", ignoreCase = true) ||
        product.contains("sdk", ignoreCase = true) ||
        product.contains("emulator", ignoreCase = true)
    if (emulator) return false
    return context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)
}

/**
 * iOS `scannerSection` + `pasteSection`. Blocks are spaced 20 like the
 * PairingView VStack; labels/controls inside each block use 10.
 */
@Composable
private fun PairFields(controller: Controller, context: android.content.Context) {
    var code by remember { mutableStateOf("") }
    var lastCode by remember { mutableStateOf<String?>(null) }
    var lastCodeAt by remember { mutableStateOf(0L) }
    val scannerSupported = remember(context) { pairingScannerAvailable(context) }
    var cameraGranted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        cameraGranted = granted
        if (!granted) controller.reportError("Camera permission is required to scan a pairing code")
    }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(20.dp)) {
        if (scannerSupported) {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(
                    t("Scan the pairing code"),
                    color = Foreground.copy(alpha = 0.85f),
                    fontSize = 14.sp,
                    fontWeight = FontWeight.SemiBold,
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
                            if (controller.busy || (scanned == lastCode && now - lastCodeAt < 2_500)) {
                                return@PairingScanner
                            }
                            lastCode = scanned
                            lastCodeAt = now
                            controller.pair(scanned)
                        }
                    } else {
                        Text(
                            t("Camera permission is required to scan a pairing code"),
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
            }
            if (!cameraGranted) {
                LaunchedEffect(Unit) { permission.launch(Manifest.permission.CAMERA) }
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                t("Or paste the pairing code"),
                color = Foreground.copy(alpha = 0.85f),
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
            )
            OutlinedTextField(
                value = code,
                onValueChange = { code = it },
                modifier = Modifier.fillMaxWidth(),
                placeholder = {
                    Text(t("Pairing code"), color = Foreground.copy(alpha = 0.35f), fontSize = 12.sp)
                },
                minLines = 3,
                maxLines = 5,
                textStyle = TextStyle(
                    color = Foreground,
                    fontSize = 12.sp,
                    fontFamily = FontFamily.Monospace,
                ),
                shape = RoundedCornerShape(10.dp),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedBorderColor = Color.Transparent,
                    unfocusedBorderColor = Color.Transparent,
                    focusedContainerColor = SoftCard,
                    unfocusedContainerColor = SoftCard,
                    cursorColor = Cyan,
                ),
            )
            val canConnect = !controller.busy && code.isNotBlank()
            Button(
                onClick = { controller.pair(code) },
                enabled = canConnect,
                modifier = Modifier.fillMaxWidth().height(44.dp),
                shape = RoundedCornerShape(10.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = Cyan,
                    contentColor = Color.White,
                    disabledContainerColor = Cyan.copy(alpha = 0.35f),
                    disabledContentColor = Color.White.copy(alpha = 0.7f),
                ),
            ) {
                if (controller.busy) {
                    CircularProgressIndicator(
                        color = Color.White,
                        strokeWidth = 2.dp,
                        modifier = Modifier.size(18.dp),
                    )
                } else {
                    Text(t("Connect"), fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                }
            }
        }
    }
}

/**
 * iOS `TerminalTopBar`: centered title/subtitle (long-press organize), sessions
 * control on the left, agent gallery + activity bell on the right.
 */
@Composable
private fun TerminalTopBar(
    session: SessionSummary,
    project: ProjectSummary?,
    galleryPulse: Boolean,
    spinning: Boolean,
    hasBlockers: Boolean,
    spinnerColor: Color,
    onSessions: () -> Unit,
    onOrganize: () -> Unit,
    onGallery: () -> Unit,
    onActivity: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val contextLabel = project?.name?.takeIf { it.isNotEmpty() }
        ?: session.presentationProviderID()
    val branch = session.worktreeBranch ?: project?.gitBranch
    // Blockers → attention. Spinning → active session tool tint (sidebar /
    // Activity row language). Idle bell → near-white primary.
    val indicator = when {
        hasBlockers -> Attention
        spinning -> spinnerColor
        else -> Foreground
    }
    // Same paddings as iOS TerminalTopBar (12 / top 10 / bottom 8); 44dp controls.
    Box(
        modifier.padding(start = 12.dp, end = 12.dp, top = 10.dp, bottom = 8.dp),
    ) {
        Column(
            Modifier
                .align(Alignment.Center)
                .widthIn(max = 160.dp)
                .combinedClickable(onClick = {}, onLongClick = onOrganize),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(
                session.title,
                color = Foreground,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (!contextLabel.isNullOrEmpty() || !branch.isNullOrEmpty()) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    if (!contextLabel.isNullOrEmpty()) {
                        Text(
                            contextLabel,
                            color = Foreground.copy(alpha = 0.55f),
                            fontSize = 11.sp,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.widthIn(max = 96.dp),
                        )
                    }
                    if (!branch.isNullOrEmpty()) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            BranchMark(9.dp, Foreground.copy(alpha = 0.55f))
                            Spacer(Modifier.width(2.dp))
                            Text(
                                branch,
                                color = Foreground.copy(alpha = 0.55f),
                                fontSize = 11.sp,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.widthIn(max = 72.dp),
                            )
                        }
                    }
                }
            }
        }
        Row(
            Modifier.fillMaxWidth().align(Alignment.Center),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            GlassButton(onClick = onSessions) {
                if (!SvgIcon("sidebar-toggle", 18.dp, tint = Foreground)) {
                    Text(t("≡"), color = Foreground, fontSize = 18.sp)
                }
            }
            Spacer(Modifier.weight(1f))
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (AgentCatalog.isAgent(session.providerID, session.command)) {
                    val pulse by animateFloatAsState(
                        if (galleryPulse) 1.08f else 1f,
                        label = "gallery",
                    )
                    Box(Modifier.graphicsLayer { scaleX = pulse; scaleY = pulse }) {
                        GlassButton(onClick = onGallery) {
                            if (!SvgIcon(
                                    "gallery",
                                    19.dp,
                                    tint = if (galleryPulse) Attention else Foreground,
                                )
                            ) {
                                Text(t("▣"), color = if (galleryPulse) Attention else Foreground, fontSize = 16.sp)
                            }
                        }
                    }
                }
                Box {
                    GlassButton(onClick = onActivity) {
                        if (spinning) {
                            BrailleSpinner(indicator)
                        } else if (!SvgIcon("bell", 17.dp, tint = indicator)) {
                            Text(t("🔔"), fontSize = 14.sp)
                        }
                    }
                    if (hasBlockers) {
                        Box(
                            Modifier
                                .align(Alignment.TopEnd)
                                .offset(x = (-4).dp, y = 4.dp)
                                .size(7.dp)
                                .clip(CircleShape)
                                .background(Attention)
                                .border(1.5.dp, Canvas, CircleShape),
                        )
                    }
                }
            }
        }
    }
}

/** Live presentation id — mirrors iOS `presentationProviderID`. */
private fun SessionSummary.presentationProviderID(): String? {
    if (status == "running") {
        activeRuntimeID?.takeIf { it.isNotBlank() }?.let { return it }
    }
    return providerID?.takeIf { it.isNotBlank() }
}

/**
 * iOS `IOSSidebarTheme.toolSpinnerColor`: Host `spinnerColorHex` first, else
 * runtime-catalog tint by provider/command (Claude terracotta, Codex purple…).
 */
@Composable
@ReadOnlyComposable
private fun toolSpinnerColor(session: SessionSummary): Color {
    session.spinnerColorHex?.let { return rgb(it) }
    val keys = listOfNotNull(
        session.activeRuntimeID?.takeIf { session.status == "running" && it.isNotBlank() },
        session.providerID?.takeIf { it.isNotBlank() },
        AgentCatalog.commandToken(session.command).takeIf { it.isNotEmpty() },
    ).map { it.trim().lowercase() }
    for (key in keys) {
        SpinnerTints[key]?.let { return Color(it) }
    }
    return GenericSpinner
}

/** Subset of `UnpeelRuntimeCatalog` spinnerTintColorHex, keyed by id/slug/alias. */
private val SpinnerTints: Map<String, Long> = mapOf(
    "com.anthropic.claude-code" to 0xFFD97757,
    "claude-code" to 0xFFD97757,
    "claude" to 0xFFD97757,
    "com.openai.codex" to 0xFFC292FE,
    "codex" to 0xFFC292FE,
    "bot.cline.cli" to 0xFF98C4FA,
    "cline" to 0xFF98C4FA,
    "com.sourcegraph.amp" to 0xFFF97316,
    "amp" to 0xFFF97316,
    "com.google.gemini-cli" to 0xFF6EA8FF,
    "gemini" to 0xFF6EA8FF,
    "dev.mariozechner.pi" to 0xFF7C95FF,
    "pi" to 0xFF7C95FF,
    "ai.opencode.cli" to 0xFF8F8787,
    "opencode" to 0xFF8F8787,
    "com.cursor.agent" to 0xFFEDEDED,
    "cursor-agent" to 0xFFEDEDED,
    "cursor" to 0xFFEDEDED,
    "ai.x.grok-cli" to 0xFF8F8787,
    "grok" to 0xFF8F8787,
    "com.moonshot.kimi-code" to 0xFFB88A2A,
    "kimi" to 0xFFB88A2A,
    "dev.kiro.cli" to 0xFFA78BFA,
    "kiro" to 0xFFA78BFA,
    "kiro-cli" to 0xFFA78BFA,
)

@Composable
private fun GlassButton(onClick: () -> Unit, content: @Composable () -> Unit) {
    // Dark keeps the near-black disc (white wash on black @ 0.42). Light uses
    // Mac hoverRow, because that disc would hide Foreground ink on white.
    val night = phoneNight()
    Box(
        Modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(if (night) Color.Black.copy(alpha = 0.42f) else Hover)
            .then(
                if (night) {
                    Modifier.background(
                        Brush.verticalGradient(
                            listOf(Color.White.copy(alpha = 0.10f), Color.White.copy(alpha = 0.03f)),
                        ),
                    )
                } else {
                    Modifier
                },
            )
            .border(1.dp, if (night) Color.White.copy(alpha = 0.10f) else Hairline, CircleShape)
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

/** iOS accessory `attach-image`: the photo glyph, not a text key. */
@Composable
private fun AttachImageKey(onClick: () -> Unit) {
    val click = rememberUpdatedState(onClick)
    Box(
        Modifier
            .size(36.dp)
            .clip(CircleShape)
            .background(KeyFill)
            .pointerInput(Unit) { detectTapGestures(onTap = { click.value() }) },
        contentAlignment = Alignment.Center,
    ) {
        PhotoMark(Foreground.copy(alpha = 0.92f))
    }
}

@Composable
private fun PhotoMark(tint: Color) {
    Canvas(Modifier.size(16.dp)) {
        val stroke = Stroke(width = 1.3.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round)
        val w = size.width
        val h = size.height
        drawRoundRect(
            tint,
            topLeft = Offset(w * 0.08f, h * 0.16f),
            size = androidx.compose.ui.geometry.Size(w * 0.84f, h * 0.68f),
            cornerRadius = androidx.compose.ui.geometry.CornerRadius(1.5.dp.toPx()),
            style = stroke,
        )
        drawCircle(tint, radius = w * 0.07f, center = Offset(w * 0.32f, h * 0.36f))
        drawLine(tint, Offset(w * 0.16f, h * 0.72f), Offset(w * 0.40f, h * 0.50f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawLine(tint, Offset(w * 0.40f, h * 0.50f), Offset(w * 0.58f, h * 0.64f), strokeWidth = stroke.width, cap = StrokeCap.Round)
        drawLine(tint, Offset(w * 0.58f, h * 0.64f), Offset(w * 0.84f, h * 0.42f), strokeWidth = stroke.width, cap = StrokeCap.Round)
    }
}

@Composable
private fun TerminalKey(label: String, textSize: Int = 14, armed: Boolean = false, onClick: () -> Unit) {
    val shape = if (label.length <= 1) CircleShape else RoundedCornerShape(percent = 50)
    val click = rememberUpdatedState(onClick)
    Box(
        Modifier
            .height(36.dp)
            .widthIn(min = 36.dp)
            .clip(shape)
            .background(if (armed) Cyan else KeyFill)
            // A clickable key would take focus and dismiss the IME.
            .pointerInput(Unit) { detectTapGestures(onTap = { click.value() }) }
            .padding(horizontal = if (label.length > 1) 10.dp else 0.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            color = if (armed) Color.Black else Foreground.copy(alpha = 0.92f),
            fontSize = textSize.sp,
            fontWeight = FontWeight.SemiBold,
            fontFamily = if (label.length == 1) FontFamily.Monospace else FontFamily.Default,
        )
    }
}

/**
 * iOS `TitlebarBrailleSpinner`. Android fonts don't treat braille as monospace
 * (glyph advance / bearings jump every frame), so we paint the 2×4 dot grid
 * ourselves into a fixed 16dp slot — no Text, no typeface.
 */
@Composable
private fun BrailleSpinner(color: Color) {
    val transition = rememberInfiniteTransition(label = "braille")
    val phase by transition.animateFloat(
        initialValue = 0f,
        targetValue = SpinnerDotMasks.size.toFloat(),
        animationSpec = infiniteRepeatable(
            animation = tween(
                durationMillis = 120 * SpinnerDotMasks.size,
                easing = LinearEasing,
            ),
            repeatMode = RepeatMode.Restart,
        ),
        label = "braillePhase",
    )
    val mask = SpinnerDotMasks[phase.toInt().floorMod(SpinnerDotMasks.size)]
    val glow = color.copy(alpha = 0.45f)
    // Fully-qualify — file-level `Canvas` color token shadows the composable.
    androidx.compose.foundation.Canvas(Modifier.size(16.dp)) {
        // Spinner only lights dots 1–6 → 2×3 grid. One shared pitch so
        // row spacing matches column spacing (old /3 vs /5 looked squat).
        val gap = minOf(size.width, size.height) / 3.2f
        val cx0 = size.width / 2f - gap / 2f
        val cy0 = size.height / 2f - gap
        val r = (gap * 0.20f).coerceAtLeast(1.0f)
        // Unicode braille order: 1 4 / 2 5 / 3 6 / 7 8
        val positions = arrayOf(
            0 to 0, 0 to 1, 0 to 2, 1 to 0, 1 to 1, 1 to 2, 0 to 3, 1 to 3,
        )
        for (i in positions.indices) {
            if (mask and (1 shl i) == 0) continue
            val (col, row) = positions[i]
            val x = cx0 + col * gap
            val y = cy0 + row * gap
            drawCircle(glow, radius = r * 1.6f, center = Offset(x, y))
            drawCircle(color, radius = r, center = Offset(x, y))
        }
    }
}

private fun Int.floorMod(m: Int): Int = ((this % m) + m) % m

/** iOS `bellBlockedSessions`. */
private fun bellApprovalIDs(approvals: List<PendingApproval>): Set<String> =
    approvals.mapNotNull { it.targetSessionID ?: it.callerSessionID.takeIf { id -> id.isNotEmpty() } }.toSet()

private fun bellBlockedSessions(
    sessions: List<SessionSummary>,
    approvalIDs: Set<String>,
): List<SessionSummary> =
    sessions
        .filter { it.status == "running" && (it.activity == "blocked" || it.id in approvalIDs) }
        .distinctBy { it.id }
        .sortedByDescending { it.updatedAtUnixMs ?: 0L }

/** iOS `bellActiveSessions` — running + working only, blockers excluded. */
private fun bellActiveSessions(
    sessions: List<SessionSummary>,
    blockerIDs: Set<String>,
): List<SessionSummary> =
    sessions
        .filter { it.status == "running" && it.activity == "working" && it.id !in blockerIDs }
        .distinctBy { it.id }
        .take(8)

@Composable
private fun AppLockCover(lock: AppLock, onUnlock: () -> Unit) {
    Column(
        Modifier.fillMaxSize().background(Canvas).clickable(onClick = {}).padding(28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(t("Locked"), color = Foreground, fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(8.dp))
        Text(t("Authenticate to keep viewing sessions"), color = Muted, fontSize = 14.sp)
        lock.lastError?.let {
            Spacer(Modifier.height(8.dp))
            Text(t(it), color = Danger, fontSize = 13.sp)
        }
        Spacer(Modifier.height(18.dp))
        Button(onClick = onUnlock, enabled = !lock.authInFlight) { Text(if (lock.authInFlight) t("Verifying…") else t("Unlock")) }
    }
}

/**
 * iOS `ExitedSessionRestartBar`: material capsule over the terminal with a
 * status line and an accent Resume pill (spinner while in flight).
 */
@Composable
private fun ExitedRestartBar(
    restarting: Boolean,
    errorText: String? = null,
    onRestart: () -> Unit,
) {
    val status = when {
        restarting -> "Resuming…"
        !errorText.isNullOrBlank() -> errorText
        else -> "Session ended"
    }
    val warn = !errorText.isNullOrBlank() && !restarting
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp)
            .shadow(14.dp, CircleShape, clip = false)
            .clip(CircleShape)
            .background(Color.Black.copy(alpha = 0.55f))
            .border(1.dp, Color.White.copy(alpha = 0.12f), CircleShape)
            .padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            if (warn) "!" else "⏹",
            color = if (warn) Attention else Color.White.copy(alpha = 0.6f),
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Spacer(Modifier.width(12.dp))
        Text(
            t(status),
            color = Color.White.copy(alpha = 0.85f),
            fontSize = 14.sp,
            fontWeight = FontWeight.Medium,
            maxLines = 2,
            modifier = Modifier.weight(1f),
        )
        Spacer(Modifier.width(8.dp))
        Row(
            Modifier
                .clip(CircleShape)
                .background(Accent.copy(alpha = if (restarting) 0.7f else 1f))
                .clickable(enabled = !restarting, onClick = onRestart)
                .padding(horizontal = 14.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (restarting) {
                CircularProgressIndicator(
                    color = Color.White,
                    strokeWidth = 2.dp,
                    modifier = Modifier.size(14.dp),
                )
            } else {
                Text(t("↻"), color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.Bold)
            }
            Text(t("Resume"), color = Color.White, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
        }
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
    val tint = folderSwatch(colorID) ?: Muted
    Row(
        Modifier.fillMaxWidth().heightIn(min = 30.dp).clickable(onClick = onToggle).padding(start = 6.dp, end = 9.dp, top = 8.dp, bottom = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        SidebarChevron(open, tint, 9.dp)
        Spacer(Modifier.width(6.dp))
        Text(name.uppercase(), color = Foreground, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (attention) {
            Spacer(Modifier.width(6.dp))
            Box(Modifier.size(6.dp).clip(CircleShape).background(Attention))
        }
        Spacer(Modifier.weight(1f))
        if (!open && count > 0) {
            SidebarCountBadge(count, fontSize = 11.sp)
        }
    }
}

@Composable
private fun SidebarFeedbackFooter() {
    val context = LocalContext.current
    // iOS: padding.bottom = max(safeBottom, 12) — not navBars + 12.
    val navBottom = WindowInsets.navigationBars.asPaddingValues().calculateBottomPadding()
    val hairline = Hairline
    Row(
        Modifier
            .fillMaxWidth()
            .drawWithContent {
                drawContent()
                drawLine(
                    hairline,
                    start = Offset.Zero,
                    end = Offset(size.width, 0f),
                    strokeWidth = 1.dp.toPx(),
                )
            }
            .clickable {
                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://github.com/orgs/unpeel-com/discussions")))
            }
            .padding(start = 14.dp, end = 14.dp, top = 12.dp, bottom = maxOf(navBottom, 12.dp)),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FeedbackMark(Muted)
        Spacer(Modifier.width(8.dp))
        Text(t("Feedback & bugs"), color = Muted, fontSize = 13.sp, fontWeight = FontWeight.Medium, modifier = Modifier.weight(1f))
        ExternalArrow(Muted.copy(alpha = 0.7f))
    }
}

@Composable
private fun SidebarDisconnected(connection: String) {
    val unpaired = connection == "Unpaired"
    val connecting = connection == "Connecting"
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 22.dp, vertical = 36.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(
            when {
                unpaired -> t("Unpaired")
                connecting -> t("Connecting…")
                else -> t("Connection lost")
            },
            color = Foreground,
            fontSize = 14.sp,
            fontWeight = FontWeight.SemiBold,
        )
        Text(
            when {
                unpaired -> t("Pair a Mac first — sessions will show up here.")
                connecting -> t("Connecting to this Mac.")
                else -> t("Reconnecting automatically. Your sessions will reappear once your Mac is reachable.")
            },
            color = Muted,
            fontSize = 12.sp,
            textAlign = TextAlign.Center,
        )
        if (!unpaired) {
            CircularProgressIndicator(color = Muted, modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        }
    }
}

/**
 * Mac `SidebarListFadeMask`. Black is the mask (keep the row), clear drops
 * it. The offscreen layer keeps that mask off the window, so the fade is
 * the sidebar surface already behind the list: light `#FFFFFF`, dark
 * `#1A1B1D` over `#121314` (`MacChrome.surface` / `appBackground`).
 */
private fun Modifier.sidebarListFade(): Modifier =
    graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
        .drawWithContent {
            drawContent()
            val top = 16.dp.toPx()
            val bottom = 34.dp.toPx()
            val h = size.height.coerceAtLeast(1f)
            drawRect(
                brush = Brush.verticalGradient(
                    colorStops = arrayOf(
                        0f to Color.Transparent,
                        (top * 0.5f / h).coerceIn(0f, 1f) to Color.Black.copy(alpha = 0.5f),
                        (top / h).coerceIn(0f, 1f) to Color.Black,
                        ((h - bottom) / h).coerceIn(0f, 1f) to Color.Black,
                        ((h - bottom * 0.52f) / h).coerceIn(0f, 1f) to Color.Black.copy(alpha = 0.65f),
                        1f to Color.Transparent,
                    ),
                ),
                blendMode = BlendMode.DstIn,
            )
        }

@Composable
@ReadOnlyComposable
private fun selectedRowTint(hue: Double?): Color {
    if (hue == null || hue.isNaN()) return ActiveRow
    val wrapped = ((hue % 360.0) + 360.0) % 360.0
    // Theme.activeRowGlassTint at full strength.
    return if (phoneNight()) {
        Color.hsv(wrapped.toFloat(), 0.55f, 0.52f, 0.30f)
    } else {
        Color.hsv(wrapped.toFloat(), 0.22f, 0.98f, 1f)
    }
}

@Composable
private fun WorkspaceTintDot(hue: Double?, size: Dp = 9.dp) {
    val fill = if (hue == null || hue.isNaN()) {
        Muted.copy(alpha = 0.4f)
    } else {
        val wrapped = ((hue % 360.0) + 360.0) % 360.0
        Color.hsv(wrapped.toFloat(), 0.62f, 0.85f)
    }
    Box(
        Modifier
            .size(size)
            .clip(CircleShape)
            .background(fill)
            .border(0.5.dp, Foreground.copy(alpha = 0.18f), CircleShape),
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

/** Session/page chrome — Mac `appBackground`, Host-tint washed. */
@Composable
@ReadOnlyComposable
private fun hostChrome(hue: Double?): Color {
    val base = MacChrome.appBackground(phoneNight()) and 0xFFFFFF
    return if (hue == null) Color(0xFF000000.toInt() or base) else Color(hostTintedArgb(base, hue))
}

/**
 * Mac `Theme.appTinted` / iOS `TerminalChrome.hostTinted`: keep the base
 * brightness, push hue to the workspace App color with a brightness-scaled
 * saturation wash. Nil hue returns the neutral hex untouched.
 */
private fun hostTintedArgb(rgb: Int, tintHue: Double?): Int {
    val base = rgb and 0xFFFFFF
    if (tintHue == null || tintHue.isNaN()) return 0xFF000000.toInt() or base
    val red = ((base shr 16) and 0xFF) / 255.0
    val green = ((base shr 8) and 0xFF) / 255.0
    val blue = (base and 0xFF) / 255.0
    val brightness = maxOf(red, green, blue)
    val delta = brightness - minOf(red, green, blue)
    val saturation = if (brightness == 0.0) 0.0 else delta / brightness
    val wash = maxOf(saturation, minOf(0.5, 0.615 - 0.59 * brightness))
    var hueUnit = (tintHue / 360.0)
    hueUnit -= kotlin.math.floor(hueUnit)
    val h = hueUnit * 6.0
    val sector = kotlin.math.floor(h)
    val f = h - sector
    val p = brightness * (1.0 - wash)
    val q = brightness * (1.0 - wash * f)
    val t = brightness * (1.0 - wash * (1.0 - f))
    val (r, g, b) = when (sector.toInt() % 6) {
        0 -> Triple(brightness, t, p)
        1 -> Triple(q, brightness, p)
        2 -> Triple(p, brightness, t)
        3 -> Triple(p, q, brightness)
        4 -> Triple(t, p, brightness)
        else -> Triple(brightness, p, q)
    }
    fun channel(v: Double): Int = (v * 255.0).toInt().coerceIn(0, 255)
    return 0xFF000000.toInt() or (channel(r) shl 16) or (channel(g) shl 8) or channel(b)
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
            "Resume agent keeps this terminal. Stop leaves the session in the list so you can resume. Remove deletes it."
        session.status == "running" && offer == ResumeOffer.Agent ->
            "Resume agent keeps this terminal. Remove stops and deletes this session."
        session.status == "running" && stop ->
            "Stop leaves the session in the list so you can resume. Remove stops and deletes it."
        session.status == "running" -> "This launch has nothing to resume. Remove stops and deletes it."
        offer == ResumeOffer.Session -> "Resume continues this conversation. Remove only clears it from the list."
        else -> "Remove only clears it from the list."
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
