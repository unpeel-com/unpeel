package com.unpeel.controller

import org.json.JSONArray
import org.json.JSONObject

data class SessionCapabilities(
    val restart: Boolean,
    val resumeAgent: Boolean?,
    val notifyWhenDone: Boolean,
    val resumable: Boolean,
)

data class SessionSummary(
    val id: String,
    val projectID: String,
    val title: String,
    val command: String,
    val status: String,
    val activity: String,
    val activitySource: String?,
    val unread: Boolean,
    val pinned: Boolean,
    val providerID: String?,
    val activeRuntimeID: String?,
    val spinnerColorHex: Int?,
    val terminalBackgroundHex: Int?,
    val lastOutputPreview: String?,
    val cwd: String?,
    val activeAppName: String?,
    val updatedAtUnixMs: Long?,
    val worktreeBranch: String?,
    val notifyWhenDone: Boolean = false,
    val capabilities: SessionCapabilities? = null,
    val runtimeLaunchPending: Boolean = false,
    val worktreePath: String? = null,
    val parentSessionID: String? = null,
)

data class ProjectSummary(
    val id: String,
    val name: String,
    val path: String,
    val folderID: String?,
    val parentProjectID: String?,
    val isGroup: Boolean,
    val gitBranch: String?,
    val worktreeBranch: String?,
    val sortOrder: Int,
    val colorID: String? = null,
    val pinned: Boolean? = null,
    val dateSorted: Boolean? = null,
    val sessionOrder: List<String>? = null,
)

data class FolderSummary(
    val id: String,
    val name: String,
    val sortOrder: Int,
    val parentFolderID: String? = null,
    val colorID: String? = null,
)

data class PresetSummary(
    val id: String,
    val label: String,
    val command: String,
    val projectID: String?,
    val enabled: Boolean,
)

data class PendingApproval(
    val id: String,
    val kind: String,
    val title: String,
    val body: String,
    val callerSessionID: String,
    val targetSessionID: String?,
)

data class WorkspaceSummary(
    val id: String,
    val name: String,
    val isCurrent: Boolean,
    val kind: String,
    /** App-color hue in degrees. Null is the neutral dot. */
    val tintHue: Double? = null,
)

data class ArtifactSummary(
    val kind: String,
    val name: String,
    val size: Long,
    val modifiedAtUnixMs: Long,
)

data class OutputChunk(
    val offset: Long,
    val nextOffset: Long,
    val data: ByteArray,
    val modePreamble: ByteArray?,
    /** The Host restarted this read from a new journal position. */
    val truncated: Boolean = false,
)

data class BootstrapSnapshot(
    val macID: String?,
    val macName: String?,
    val serverVersion: String?,
    val directEndpoint: String?,
    val remoteServerPort: Int?,
    val remoteServerCertificateFingerprint: String?,
    val capabilities: List<String>,
    val folders: List<FolderSummary>,
    val projects: List<ProjectSummary>,
    val presets: List<PresetSummary>,
    val sessions: List<SessionSummary>,
    val pendingApprovals: List<PendingApproval>,
    val workspaces: List<WorkspaceSummary>,
    val hostDeviceModel: String?,
    val hostTintHue: Double? = null,
    val experimentalWorktreesEnabled: Boolean = false,
)

object BootstrapJson {
    fun parse(body: String): BootstrapSnapshot {
        val json = JSONObject(body)
        val hostProtocol = json.optJSONObject("hostProtocol")
        val capabilities = hostProtocol?.optJSONArray("capabilities").toStrings()
        return BootstrapSnapshot(
            macID = json.optString("macID").ifEmpty { null },
            macName = json.optString("macName").ifEmpty { null },
            serverVersion = json.optString("serverVersion").ifEmpty { null },
            directEndpoint = json.optString("directEndpoint").ifEmpty { null },
            remoteServerPort = json.optInt("remoteServerPort").takeIf { json.has("remoteServerPort") && !json.isNull("remoteServerPort") },
            remoteServerCertificateFingerprint = json.optString("remoteServerCertificateFingerprint").ifEmpty { null },
            capabilities = capabilities,
            folders = json.optJSONArray("folders").objects().map {
                FolderSummary(
                    id = it.getString("id"),
                    name = it.optString("name"),
                    sortOrder = it.optInt("sortOrder"),
                    parentFolderID = it.optString("parentFolderID").ifEmpty { null },
                    colorID = it.optString("colorID").ifEmpty { null },
                )
            },
            // Host array order is the sidebar order, same as iOS. sortOrder
            // can still be the file rank from before a drag, so re-sorting
            // by it scrambles the list the Mac already published.
            projects = json.optJSONArray("projects").objects().map {
                ProjectSummary(
                    id = it.getString("id"),
                    name = it.optString("name"),
                    path = it.optString("path"),
                    folderID = it.optString("folderID").ifEmpty { null },
                    parentProjectID = it.optString("parentProjectID").ifEmpty { null },
                    isGroup = it.optBoolean("isGroup"),
                    gitBranch = it.optString("gitBranch").ifEmpty { null },
                    worktreeBranch = it.optString("worktreeBranch").ifEmpty { null },
                    sortOrder = it.optInt("sortOrder"),
                    colorID = it.optString("colorID").ifEmpty { null },
                    pinned = if (it.has("pinned") && !it.isNull("pinned")) it.optBoolean("pinned") else null,
                    dateSorted = if (it.has("dateSorted") && !it.isNull("dateSorted")) it.optBoolean("dateSorted") else null,
                    sessionOrder = it.optJSONArray("sessionOrder")?.let { order ->
                        (0 until order.length()).map { index -> order.optString(index) }.filter { id -> id.isNotEmpty() }
                    },
                )
            },
            presets = json.optJSONArray("presets").objects().map {
                PresetSummary(
                    id = it.getString("id"),
                    label = it.optString("label"),
                    command = it.optString("command"),
                    projectID = it.optString("projectID").ifEmpty { null },
                    enabled = if (it.has("enabled")) it.optBoolean("enabled") else true,
                )
            },
            sessions = json.optJSONArray("sessions").objects().map { session(it) },
            pendingApprovals = json.optJSONArray("pendingApprovals").objects().map {
                PendingApproval(
                    id = it.getString("id"),
                    kind = it.optString("kind"),
                    title = it.optString("title"),
                    body = it.optString("body"),
                    callerSessionID = it.optString("callerSessionID"),
                    targetSessionID = it.optString("targetSessionID").ifEmpty { null },
                )
            },
            workspaces = json.optJSONArray("hostWorkspaces").objects().map {
                WorkspaceSummary(
                    id = it.getString("id"),
                    name = it.optString("name"),
                    isCurrent = it.optBoolean("isCurrent"),
                    kind = it.optString("kind").ifEmpty { "local" },
                    tintHue = if (it.has("tintHue") && !it.isNull("tintHue")) it.optDouble("tintHue") else null,
                )
            },
            hostDeviceModel = json.optString("hostDeviceModel").ifEmpty { null },
            hostTintHue = if (json.has("hostTintHue") && !json.isNull("hostTintHue")) json.optDouble("hostTintHue") else null,
            experimentalWorktreesEnabled = json.optBoolean("experimentalWorktreesEnabled"),
        )
    }

    fun outputChunk(body: String): OutputChunk {
        val json = JSONObject(body)
        return OutputChunk(
            offset = json.optLong("offset"),
            nextOffset = json.optLong("nextOffset"),
            data = b64d(json.optString("dataBase64")),
            modePreamble = json.optString("modePreambleBase64").ifEmpty { null }?.let { b64d(it) },
            truncated = json.optBoolean("truncated", false),
        )
    }

    fun artifacts(body: String): List<ArtifactSummary> =
        JSONObject(body).optJSONArray("artifacts").objects().map {
            ArtifactSummary(
                kind = it.optString("kind"),
                name = it.optString("name"),
                size = it.optLong("size"),
                modifiedAtUnixMs = it.optLong("modifiedAtUnixMs"),
            )
        }

    fun artifactChunk(body: String): Pair<ByteArray, Long> {
        val json = JSONObject(body)
        return b64d(json.optString("dataBase64")) to json.optLong("totalSize")
    }

    private fun session(it: JSONObject) = SessionSummary(
        id = it.getString("id"),
        projectID = it.optString("projectID"),
        title = it.optString("title"),
        command = it.optString("command"),
        status = it.optString("status", "running"),
        activity = it.optString("activity", "unknown"),
        activitySource = it.optString("activitySource").ifEmpty { null },
        unread = it.optBoolean("unread"),
        pinned = it.optBoolean("pinned"),
        providerID = it.optString("providerID").ifEmpty { null },
        activeRuntimeID = it.optString("activeRuntimeID").ifEmpty { null },
        spinnerColorHex = if (it.has("spinnerColorHex") && !it.isNull("spinnerColorHex")) it.optInt("spinnerColorHex") else null,
        terminalBackgroundHex = if (it.has("terminalBackgroundHex") && !it.isNull("terminalBackgroundHex")) it.optInt("terminalBackgroundHex") else null,
        lastOutputPreview = it.optString("lastOutputPreview").ifEmpty { null },
        cwd = it.optString("cwd").ifEmpty { null },
        activeAppName = it.optString("activeAppName").ifEmpty { null },
        updatedAtUnixMs = when {
            it.has("updatedAtUnixMs") && !it.isNull("updatedAtUnixMs") -> it.optLong("updatedAtUnixMs")
            it.has("createdAtUnixMs") && !it.isNull("createdAtUnixMs") -> it.optLong("createdAtUnixMs")
            else -> null
        },
        worktreeBranch = it.optString("worktreeBranch").ifEmpty { null },
        notifyWhenDone = it.optBoolean("notifyWhenDone"),
        capabilities = sessionCapabilities(it),
        runtimeLaunchPending = it.optBoolean("runtimeLaunchPending"),
        worktreePath = it.optString("worktreePath").ifEmpty { null },
        parentSessionID = it.optString("parentSessionID").ifEmpty { null },
    )

    private fun sessionCapabilities(it: JSONObject): SessionCapabilities? {
        val raw = it.optJSONObject("capabilities") ?: return null
        val resumable = when {
            raw.has("resumable") && !raw.isNull("resumable") -> raw.optBoolean("resumable")
            raw.has("archive") && !raw.isNull("archive") -> raw.optBoolean("archive")
            else -> false
        }
        return SessionCapabilities(
            restart = raw.optBoolean("restart"),
            resumeAgent = if (raw.has("resumeAgent") && !raw.isNull("resumeAgent")) raw.optBoolean("resumeAgent") else null,
            notifyWhenDone = if (raw.has("notifyWhenDone") && !raw.isNull("notifyWhenDone")) raw.optBoolean("notifyWhenDone") else false,
            resumable = resumable,
        )
    }
}

private fun JSONArray?.objects(): List<JSONObject> {
    if (this == null) return emptyList()
    return (0 until length()).map { getJSONObject(it) }
}

private fun JSONArray?.toStrings(): List<String> {
    if (this == null) return emptyList()
    return (0 until length()).map { getString(it) }
}

/** What the session sheet may offer. Missing capabilities fail closed. */
enum class ResumeOffer { None, Agent, Session }

fun resumeOffer(session: SessionSummary, hostCapabilities: List<String>): ResumeOffer {
    if (session.status == "running") {
        val canResumeAgent = session.activeRuntimeID.isNullOrBlank() &&
            !session.runtimeLaunchPending &&
            session.capabilities?.resumeAgent == true &&
            "session.runtime.resume" in hostCapabilities
        return if (canResumeAgent) ResumeOffer.Agent else ResumeOffer.None
    }
    return if (session.capabilities?.restart == true) ResumeOffer.Session else ResumeOffer.None
}

fun canStopSession(session: SessionSummary): Boolean =
    session.status == "running" && session.capabilities?.resumable == true

/** Notify toggle stays available when an older Host omits the capabilities object. */
fun canNotifyWhenDone(session: SessionSummary): Boolean =
    session.capabilities?.notifyWhenDone ?: true

/** Dark folder palette shared with the Mac and iOS sidebars. Null stays neutral. */
fun folderColorArgb(id: String?): Int? = when (id) {
    "sky" -> 0xFF7DD3FC.toInt()
    "blue" -> 0xFF7EA6FF.toInt()
    "violet" -> 0xFFB79CFF.toInt()
    "rose" -> 0xFFF79AC0.toInt()
    "amber" -> 0xFFF8C86A.toInt()
    "moss" -> 0xFF9DD67A.toInt()
    "teal" -> 0xFF64DCCB.toInt()
    "graphite" -> 0xFFB8BCC8.toInt()
    else -> null
}

/**
 * Launch identity only. A runtime seen inside a plain shell may borrow a logo,
 * but not the agent gallery or the transcript action.
 */
object AgentCatalog {
    private val transcript = setOf(
        "com.anthropic.claude-code", "claude-code", "claude",
        "com.openai.codex", "codex",
        "bot.cline.cli", "cline",
        "com.google.gemini-cli", "gemini",
        "ai.opencode.cli", "opencode",
        "com.cursor.agent", "cursor-agent", "cursor",
        "ai.x.grok-cli", "grok",
        "com.moonshot.kimi-code", "kimi",
        "dev.kiro.cli", "kiro", "kiro-cli",
        "ai.meta.muse-code", "muse-code", "muse",
        "sh.omp.cli", "omp",
    )
    private val agents = transcript + setOf(
        "com.sourcegraph.amp", "amp",
        "dev.mariozechner.pi", "pi",
        "com.github.copilot-cli", "github-copilot", "copilot",
        "com.vercel.fx", "fx",
        "com.google.antigravity-cli", "antigravity", "agy",
    )

    fun commandToken(command: String): String {
        val word = command.trim().substringBefore(' ').trim('\'', '"')
        return word.substringAfterLast('/').substringAfterLast('\\').lowercase()
    }

    fun isAgent(providerID: String?, command: String): Boolean =
        keys(providerID, command).any { it in agents }

    fun supportsTranscript(providerID: String?, command: String): Boolean =
        keys(providerID, command).any { it in transcript }

    private fun keys(providerID: String?, command: String): List<String> = listOfNotNull(
        providerID?.trim()?.lowercase()?.takeIf { it.isNotEmpty() },
        commandToken(command).takeIf { it.isNotEmpty() },
    )
}

fun isInlineSidebarProject(project: ProjectSummary): Boolean =
    project.parentProjectID != null && (project.isGroup || project.worktreeBranch != null)

fun isProjectSidebarGroup(project: ProjectSummary): Boolean =
    project.isGroup && project.parentProjectID != null && project.id == "sidebar-${project.parentProjectID}"

data class FolderGroup(val folder: FolderSummary, val projects: List<ProjectSummary>)

data class SidebarIndex(
    val folderGroups: List<FolderGroup>,
    val looseProjects: List<ProjectSummary>,
    val sessionsByProject: Map<String, List<SessionSummary>>,
    val childrenByParent: Map<String, List<ProjectSummary>>,
    val hoistedSessionIDs: Set<String>,
)

sealed interface SidebarEntry {
    data class Session(val session: SessionSummary) : SidebarEntry
    data class Project(val project: ProjectSummary) : SidebarEntry
}

fun sidebarIndex(snapshot: BootstrapSnapshot): SidebarIndex {
    val folderIDs = snapshot.folders.map { it.id }.toSet()
    fun isMainTree(project: ProjectSummary, legacyFolderID: String?): Boolean {
        if (isInlineSidebarProject(project)) return false
        val parent = project.parentProjectID ?: return true
        if (parent == legacyFolderID) return true
        return parent in folderIDs
    }
    val loose = snapshot.projects.filter { it.folderID == null && isMainTree(it, null) }
    val groups = snapshot.folders.mapNotNull { folder ->
        val projects = snapshot.projects.filter {
            it.folderID == folder.id && isMainTree(it, folder.id)
        }
        if (projects.isEmpty()) null else FolderGroup(folder, projects)
    }
    val sidebarParents = snapshot.projects.mapNotNull { project ->
        if (!isProjectSidebarGroup(project)) null
        else project.parentProjectID?.let { project.id to it }
    }.toMap()
    val hoisted = snapshot.sessions.mapNotNull { session ->
        if (sidebarParents[session.projectID] != null) session.id else null
    }.toSet()
    val sessions = snapshot.sessions.groupBy { sidebarParents[it.projectID] ?: it.projectID }
    val children = snapshot.projects
        .filter { isInlineSidebarProject(it) && !isProjectSidebarGroup(it) }
        .groupBy { it.parentProjectID.orEmpty() }
    return SidebarIndex(groups, loose, sessions, children, hoisted)
}

/** Pinned rows first, then child folders and sessions in the Host's order. */
fun sidebarEntries(
    project: ProjectSummary,
    sessions: List<SessionSummary>,
    children: List<ProjectSummary>,
    hoisted: Set<String>,
    worktreesEnabled: Boolean,
): List<SidebarEntry> {
    val visibleChildren = children.filter { child ->
        child.isGroup || (worktreesEnabled && child.worktreeBranch != null)
    }
    val pinnedSessions = sessions.filter { it.pinned || it.id in hoisted }
    val pinnedIDs = pinnedSessions.map { it.id }.toSet()
    val regularSessions = sessions.filter { it.id !in pinnedIDs }.let { list ->
        if (project.dateSorted == true) list.sortedByDescending { it.updatedAtUnixMs ?: 0L } else list
    }
    val pinnedChildren = visibleChildren.filter { it.pinned == true }
    val regularChildren = visibleChildren.filter { it.pinned != true }
    return pinnedSessions.map { SidebarEntry.Session(it) } +
        pinnedChildren.map { SidebarEntry.Project(it) } +
        mixedRegular(regularSessions, regularChildren, project.sessionOrder, project.dateSorted == true)
}

private fun mixedRegular(
    sessions: List<SessionSummary>,
    folders: List<ProjectSummary>,
    order: List<String>?,
    dateSorted: Boolean,
): List<SidebarEntry> {
    if (folders.isEmpty()) return sessions.map { SidebarEntry.Session(it) }
    val folderIDs = folders.map { it.id }.toSet()
    val ranked = order.orEmpty()
    val mixed = ranked.any { it in folderIDs }
    if (dateSorted || !mixed) {
        return folders.map { SidebarEntry.Project(it) } + sessions.map { SidebarEntry.Session(it) }
    }
    val byID = LinkedHashMap<String, SidebarEntry>()
    folders.forEach { byID[it.id] = SidebarEntry.Project(it) }
    sessions.forEach { byID[it.id] = SidebarEntry.Session(it) }
    val seen = HashSet<String>()
    val rankedEntries = ranked.mapNotNull { id ->
        if (!seen.add(id)) null else byID[id]
    }
    val unranked = sessions.filter { it.id !in seen }.map { SidebarEntry.Session(it) } +
        folders.filter { it.id !in seen }.map { SidebarEntry.Project(it) }
    return unranked + rankedEntries
}

object TerminalInput {
    fun key(name: String): String = when (name) {
        "enter" -> "\r"
        "escape" -> "\u001b"
        "tab" -> "\t"
        "arrowUp" -> "\u001b[A"
        "arrowDown" -> "\u001b[B"
        "arrowRight" -> "\u001b[C"
        "arrowLeft" -> "\u001b[D"
        "controlC" -> "\u0003"
        "controlD" -> "\u0004"
        "controlZ" -> "\u001a"
        else -> ""
    }

    /** Bracketed paste, no Enter. Voice and image attach use this. */
    fun paste(text: String): String = "\u001b[200~$text\u001b[201~"

    /**
     * Paste and a carriage return in one write. Agent TUIs swallow that
     * return, so a composer submit must paste, wait, then send Enter alone.
     */
    fun pasteAndSubmit(text: String): String = paste(text) + "\r"

    /** Drop terminal controls. Newlines and tabs stay, so a multi-line note pastes as one prompt. */
    fun sanitize(text: String): String =
        text.replace("\r\n", "\n").filter { it == '\n' || it == '\t' || !it.isISOControl() }

    /** File paths make some TUIs attach before the prompt can be submitted. */
    fun containsPath(text: String): Boolean =
        text.split(Regex("\\s+")).any { it.length > 1 && '/' in it }
}
