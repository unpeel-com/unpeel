package com.unpeel.android

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.foundation.Image
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import com.unpeel.controller.SessionSummary

private val Working = Color(0xFF22C55E)
private val Blocked = Color(0xFFF59E0B)
private val Done = Color(0xFF3B82F6)

@Composable
fun UnpeelApp(controller: Controller) {
    MaterialTheme(colorScheme = darkColorScheme()) {
        if (controller.openSessionID != null) {
            TerminalScreen(controller)
        } else if (controller.selectedHost == null) {
            PairScreen(controller)
        } else {
            InboxScreen(controller)
        }
    }
}

@Composable
private fun PairScreen(controller: Controller) {
    var code by remember { mutableStateOf("") }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) controller.reportError("需要相机权限才能扫配对码")
    }
    val scan = rememberLauncherForActivityResult(ScanContract()) { result ->
        result.contents?.let { controller.pair(it) }
    }
    val context = androidx.compose.ui.platform.LocalContext.current
    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("配对 Unpeel", style = MaterialTheme.typography.headlineMedium)
        Text("扫描 Mac 上的配对码，或把 UNPEEL: 开头的文本贴进来。这台手机只控制 Host，不在本地开终端。")
        OutlinedTextField(
            value = code,
            onValueChange = { code = it },
            modifier = Modifier.fillMaxWidth(),
            label = { Text("配对码") },
            minLines = 3,
        )
        Button(onClick = { controller.pair(code) }, enabled = !controller.busy && code.isNotBlank()) { Text("配对") }
        Button(onClick = {
            val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED
            if (granted) {
                scan.launch(ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE).setPrompt("对准 Unpeel 配对码"))
            } else {
                permission.launch(Manifest.permission.CAMERA)
            }
        }) { Text("扫二维码") }
        controller.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun InboxScreen(controller: Controller) {
    val snapshot = controller.snapshot
    var showPair by remember { mutableStateOf(false) }
    Scaffold { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), horizontalArrangement = Arrangement.SpaceBetween) {
                Column {
                    Text(controller.selectedHost?.macName ?: "Unpeel", style = MaterialTheme.typography.titleLarge)
                    Text(controller.connection, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Row {
                    TextButton(onClick = { controller.showCreate = true }) { Text("新建") }
                    TextButton(onClick = { showPair = true }) { Text("配对") }
                }
            }
            controller.error?.let {
                TextButton(onClick = controller::dismissError) { Text(it, color = MaterialTheme.colorScheme.error) }
            }
            snapshot?.pendingApprovals.orEmpty().forEach { approval ->
                ApprovalCard(approval.title, approval.body, onAllow = { controller.answer(approval.id, true) }, onDeny = { controller.answer(approval.id, false) })
            }
            snapshot?.workspaces.orEmpty().filter { it.kind == "local" }.takeIf { it.size > 1 }?.let { workspaces ->
                Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp)) {
                    workspaces.forEach { workspace ->
                        TextButton(onClick = { controller.selectWorkspace(workspace.id) }) {
                            Text(if (workspace.isCurrent) "● ${workspace.name}" else workspace.name)
                        }
                    }
                }
            }
            if (snapshot == null) {
                Text("正在读取会话…", Modifier.padding(16.dp))
            } else {
                LazyColumn(Modifier.fillMaxSize()) {
                    val grouped = snapshot.sessions.groupBy { it.projectID }
                    val projects = snapshot.projects.sortedWith(compareBy({ it.folderID ?: "" }, { it.sortOrder }, { it.name }))
                    projects.forEach { project ->
                        val sessions = grouped[project.id].orEmpty().sortedWith(compareByDescending<SessionSummary> { it.activity == "blocked" }.thenByDescending { it.pinned })
                        if (sessions.isEmpty() && project.isGroup) return@forEach
                        item(key = "project-${project.id}") {
                            Text(
                                buildString {
                                    append(project.name)
                                    project.gitBranch?.let { append("  ·  ").append(it) }
                                },
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                style = MaterialTheme.typography.labelLarge,
                            )
                        }
                        items(sessions, key = { it.id }) { session ->
                            SessionRow(session) { controller.openSession(session.id) }
                        }
                    }
                }
            }
        }
    }
    if (showPair) {
        AlertDialog(
            onDismissRequest = { showPair = false },
            confirmButton = {},
            title = { Text("再配一台 Host") },
            text = { PairScreen(controller) },
        )
    }
    if (controller.showCreate) {
        CreateDialog(controller)
    }
}

@Composable
private fun SessionRow(session: SessionSummary, onOpen: () -> Unit) {
    TextButton(onClick = onOpen, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(activityMark(session), color = activityColor(session), fontFamily = FontFamily.Monospace)
            Column(Modifier.weight(1f)) {
                Text(session.title.ifEmpty { session.command }, maxLines = 1)
                Text(
                    session.activeAppName ?: session.activeRuntimeID ?: session.providerID ?: session.command,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
            if (session.unread) Text("●", color = Done)
        }
    }
}

@Composable
private fun TerminalScreen(controller: Controller) {
    val session = controller.snapshot?.sessions?.firstOrNull { it.id == controller.openSessionID }
    val revision = controller.screenRevision
    val lines = remember(revision) { controller.screen.snapshot() }
    var menu by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var titleDraft by remember { mutableStateOf("") }
    var confirmingRemove by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxSize().background(Color(0xFF111111))) {
        Row(Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = controller::closeSession) { Text("会话") }
            Text(session?.title ?: "终端", modifier = Modifier.weight(1f), maxLines = 1)
            TextButton(onClick = controller::requestScreenshot) { Text("截图") }
            TextButton(onClick = controller::loadArtifacts) { Text("图库") }
            TextButton(onClick = { menu = true }) { Text("更多") }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text(if (session?.pinned == true) "取消置顶" else "置顶") },
                    onClick = {
                        menu = false
                        session?.let { controller.organize(it.id, pinned = !it.pinned) }
                    },
                )
                DropdownMenuItem(
                    text = { Text("重命名") },
                    onClick = {
                        menu = false
                        titleDraft = session?.title.orEmpty()
                        renaming = true
                    },
                )
                DropdownMenuItem(
                    text = { Text("移除") },
                    onClick = {
                        menu = false
                        confirmingRemove = true
                    },
                )
            }
        }
        session?.let {
            if (it.activity == "blocked") {
                controller.snapshot?.pendingApprovals?.filter { approval ->
                    approval.targetSessionID == it.id || approval.callerSessionID == it.id
                }?.forEach { approval ->
                    ApprovalCard(approval.title, approval.body, { controller.answer(approval.id, true) }, { controller.answer(approval.id, false) })
                }
            }
        }
        Column(
            Modifier
                .weight(1f)
                .horizontalScroll(rememberScrollState())
                .verticalScroll(rememberScrollState())
                .padding(8.dp),
        ) {
            lines.forEach { row ->
                Text(
                    buildAnnotatedString {
                        row.forEach { cell ->
                            withStyle(
                                SpanStyle(
                                    color = Color(cell.fg),
                                    background = Color(cell.bg),
                                    fontSize = 13.sp,
                                    fontFamily = FontFamily.Monospace,
                                ),
                            ) { append(if (cell.ch == '\u0000') ' ' else cell.ch) }
                        }
                    },
                    fontFamily = FontFamily.Monospace,
                    fontSize = 13.sp,
                )
            }
        }
        Row(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 8.dp)) {
            listOf(
                "Esc" to "escape",
                "Tab" to "tab",
                "↑" to "arrowUp",
                "↓" to "arrowDown",
                "←" to "arrowLeft",
                "→" to "arrowRight",
                "Ctrl-C" to "controlC",
            ).forEach { (label, name) ->
                TextButton(onClick = { controller.sendKey(name) }) { Text(label) }
            }
            TextButton(onClick = { controller.sessionAction(controller.openSessionID!!, "stop") }) { Text("停止") }
            TextButton(onClick = { controller.sessionAction(controller.openSessionID!!, "restart") }) { Text("重启") }
        }
        Row(Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = controller.composer,
                onValueChange = { controller.composer = it },
                modifier = Modifier.weight(1f),
                placeholder = { Text("发给这个会话") },
            )
            Button(onClick = controller::sendComposer) { Text("发送") }
        }
    }
    if (renaming) {
        AlertDialog(
            onDismissRequest = { renaming = false },
            confirmButton = {
                TextButton(
                    onClick = {
                        session?.let { controller.organize(it.id, title = titleDraft.trim()) }
                        renaming = false
                    },
                    enabled = titleDraft.isNotBlank(),
                ) { Text("保存") }
            },
            dismissButton = { TextButton(onClick = { renaming = false }) { Text("取消") } },
            title = { Text("重命名") },
            text = { OutlinedTextField(value = titleDraft, onValueChange = { titleDraft = it }, singleLine = true) },
        )
    }
    if (confirmingRemove) {
        AlertDialog(
            onDismissRequest = { confirmingRemove = false },
            confirmButton = {
                TextButton(onClick = {
                    confirmingRemove = false
                    controller.openSessionID?.let { controller.sessionAction(it, "remove") }
                }) { Text("移除") }
            },
            dismissButton = { TextButton(onClick = { confirmingRemove = false }) { Text("取消") } },
            title = { Text("移除这个会话") },
            text = { Text("会话会从列表里消失。正在运行的进程会先停掉。") },
        )
    }
    if (controller.showGallery) GalleryDialog(controller)
    controller.artifactImage?.let { bitmap ->
        AlertDialog(
            onDismissRequest = controller::dismissArtifact,
            confirmButton = { TextButton(onClick = controller::dismissArtifact) { Text("关闭") } },
            text = { Image(bitmap = bitmap.asImageBitmap(), contentDescription = null) },
        )
    }
}

@Composable
private fun GalleryDialog(controller: Controller) {
    AlertDialog(
        onDismissRequest = { controller.showGallery = false },
        confirmButton = { TextButton(onClick = { controller.showGallery = false }) { Text("关闭") } },
        title = { Text("截图和图库") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                if (controller.artifacts.isEmpty()) Text("还没有截图。")
                controller.artifacts.forEach { artifact ->
                    TextButton(onClick = { controller.openArtifact(artifact.kind, artifact.name) }) {
                        Text("${artifact.kind}  ${artifact.name}")
                    }
                }
            }
        },
    )
}

@Composable
private fun CreateDialog(controller: Controller) {
    val snapshot = controller.snapshot
    var projectID by remember { mutableStateOf(snapshot?.projects?.firstOrNull { !it.isGroup }?.id.orEmpty()) }
    var presetID by remember { mutableStateOf(snapshot?.presets?.firstOrNull { it.enabled }?.id) }
    var text by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = { controller.showCreate = false },
        confirmButton = {
            TextButton(
                onClick = { controller.createSession(projectID, presetID, text) },
                enabled = projectID.isNotEmpty() && !controller.busy,
            ) { Text("创建") }
        },
        dismissButton = { TextButton(onClick = { controller.showCreate = false }) { Text("取消") } },
        title = { Text("新建会话") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("项目")
                snapshot?.projects?.filter { !it.isGroup }?.forEach { project ->
                    TextButton(onClick = { projectID = project.id }) {
                        Text(if (project.id == projectID) "● ${project.name}" else project.name)
                    }
                }
                Text("预设")
                snapshot?.presets?.filter { it.enabled }?.forEach { preset ->
                    TextButton(onClick = { presetID = preset.id }) {
                        Text(if (preset.id == presetID) "● ${preset.label}" else preset.label)
                    }
                }
                OutlinedTextField(value = text, onValueChange = { text = it }, label = { Text("开头的一句话，可空") })
            }
        },
    )
}

@Composable
private fun ApprovalCard(title: String, body: String, onAllow: () -> Unit, onDeny: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        Text(title, color = Blocked)
        if (body.isNotEmpty()) Text(body, style = MaterialTheme.typography.bodySmall)
        Row {
            TextButton(onClick = onAllow) { Text("允许") }
            TextButton(onClick = onDeny) { Text("拒绝") }
        }
    }
}

private fun activityMark(session: SessionSummary): String = when {
    session.status == "exited" -> "·"
    session.activity == "working" || session.activity == "starting" -> "●"
    session.activity == "blocked" -> "!"
    session.activity == "done" -> "✓"
    else -> " "
}

private fun activityColor(session: SessionSummary): Color = when (session.activity) {
    "working", "starting" -> session.spinnerColorHex?.let { Color(it) } ?: Working
    "blocked" -> Blocked
    "done" -> Done
    else -> Color(0xFF737373)
}
