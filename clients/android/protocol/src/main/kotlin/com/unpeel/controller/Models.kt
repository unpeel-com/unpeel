package com.unpeel.controller

import org.json.JSONArray
import org.json.JSONObject

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
)

data class FolderSummary(
    val id: String,
    val name: String,
    val sortOrder: Int,
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
                )
            }.sortedBy { it.sortOrder },
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
                )
            }.sortedBy { it.sortOrder },
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
                )
            },
            hostDeviceModel = json.optString("hostDeviceModel").ifEmpty { null },
        )
    }

    fun outputChunk(body: String): OutputChunk {
        val json = JSONObject(body)
        return OutputChunk(
            offset = json.optLong("offset"),
            nextOffset = json.optLong("nextOffset"),
            data = b64d(json.optString("dataBase64")),
            modePreamble = json.optString("modePreambleBase64").ifEmpty { null }?.let { b64d(it) },
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
    )
}

private fun JSONArray?.objects(): List<JSONObject> {
    if (this == null) return emptyList()
    return (0 until length()).map { getJSONObject(it) }
}

private fun JSONArray?.toStrings(): List<String> {
    if (this == null) return emptyList()
    return (0 until length()).map { getString(it) }
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

    /** Bracketed paste plus Enter, the same recipe the iOS composer uses. */
    fun pasteAndSubmit(text: String): String = paste(text) + "\r"
}
