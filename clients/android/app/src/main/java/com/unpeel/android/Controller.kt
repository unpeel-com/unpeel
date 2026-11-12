package com.unpeel.android

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.BitmapFactory
import android.os.Build
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.unpeel.controller.AgentCatalog
import com.unpeel.controller.ArtifactSummary
import com.unpeel.controller.BootstrapJson
import com.unpeel.controller.BootstrapSnapshot
import com.unpeel.controller.DeviceIdentity
import com.unpeel.controller.PairingCode
import com.unpeel.controller.PairingCrypto
import com.unpeel.controller.acceptPairingResponse
import com.unpeel.controller.TerminalInput
import com.unpeel.controller.TerminalOutputBinary
import com.unpeel.controller.TerminalOutputFrames
import com.unpeel.controller.TerminalOutputText
import com.unpeel.controller.TerminalQueryFilter
import com.unpeel.controller.b64d
import com.unpeel.controller.sha256Hex
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.TimeUnit

data class SavedHost(
    val macID: String,
    val macName: String,
    val endpoint: String,
    val authToken: String,
    val fingerprint: String?,
    val relayURL: String,
    val relayToken: String,
    val e2eKeyB64: String,
    val serverVersion: String?,
    val publicEndpoint: String? = null,
)

private const val SESSION_CACHE_LIMIT = 3

class Controller(context: Context) {
    private val appContext = context.applicationContext
    private val prefs = appContext.getSharedPreferences("unpeel", Context.MODE_PRIVATE)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var pollJob: Job? = null
    private var outputJob: Job? = null
    /** Bumped every time the output loop restarts, so a cancelled loop cannot paint over the new one. */
    private var outputGeneration = 0
    private var link: HostLink? = null
    private var phoneFitGeneration = 0
    private var phoneFitSession: String? = null
    private var phoneFitColumns: Int? = null
    private var phoneFitRows: Int? = null
    private val phoneFitLock = Mutex()
    /** Keystrokes and wheel reports share one drain, in order. Bytes that arrive during a send leave together. */
    private val writeLock = Any()
    private val writeQueue = ArrayDeque<WriteBatch>()
    private var writeDraining = false

    private class WriteBatch(val sessionID: String, val data: StringBuilder)
    private var fitWatchJob: Job? = null
    private var galleryWatchJob: Job? = null
    private var galleryBaseline: Long? = null
    private var desiredColumns = 0
    private var desiredRows = 0
    private var keyboardOpen = false
    private var lastFitAttemptMs = 0L
    private var fitRejected = false
    var usingRelay by mutableStateOf(false)
        private set
    /** The public address is the live dial. A later LAN probe switches back. */
    private var usingPublic = false
    /** Address the current direct link actually dialed. */
    private var dialEndpoint: String? = null
    /**
     * Dial the live terminal stream was subscribed on. Recorded after a
     * successful bootstrap. A later failure does not clear it, so reconnecting
     * the same path does not redraw. The first success only records it.
     */
    private var outputDial: OutputDial? = null

    private data class OutputDial(val relay: Boolean, val endpoint: String?)

    var hosts by mutableStateOf(loadHosts())
        private set
    var selectedMacID by mutableStateOf(prefs.getString("selected", null))
        private set
    var snapshot by mutableStateOf<BootstrapSnapshot?>(null)
        private set
    var connection by mutableStateOf(if (hosts.isEmpty()) "未配对" else "正在连接")
        private set
    var openSessionID by mutableStateOf<String?>(null)
        private set
    /** Covers a session until its first phone-grid replay has painted. A cached session skips this. */
    var sessionPreparing by mutableStateOf(false)
        private set
    /** The current opening replay has fed a chunk. A newer replay clears it. */
    private var openingFrameReady = false
    var terminal by mutableStateOf(newTerminal())
        private set

    private class CachedSession(
        val id: String,
        val terminal: HostTerminal,
        var outputOffset: Long? = null,
        var replayDone: Boolean = false,
        /** False while a chunk is only partly fed, so the next open replays instead of doubling it. */
        var coherent: Boolean = false,
    )

    /** Newest at the end. Keeps the last painted screen and journal offset, same limit as iOS. */
    private val sessionCache = ArrayDeque<CachedSession>()
    var busy by mutableStateOf(false)
        private set
    var error by mutableStateOf<String?>(null)
        private set
    var artifacts by mutableStateOf<List<ArtifactSummary>>(emptyList())
        private set
    var artifactImage by mutableStateOf<android.graphics.Bitmap?>(null)
        private set
    var showCreate by mutableStateOf(false)
    var showGallery by mutableStateOf(false)
    var galleryPulse by mutableStateOf(false)
        private set
    var restartingSessionID by mutableStateOf<String?>(null)
        private set

    val deviceId: String
        get() = prefs.getString("deviceId", null) ?: UUID.randomUUID().toString().also {
            prefs.edit().putString("deviceId", it).apply()
        }

    val selectedHost: SavedHost?
        get() = hosts.firstOrNull { it.macID == selectedMacID } ?: hosts.firstOrNull()

    init {
        if (selectedHost != null) startPolling()
    }

    fun pair(raw: String) {
        val payload = PairingCode.decode(raw) ?: run {
            error = "无法识别配对码"
            return
        }
        if (payload.expiresAtUnixMs <= System.currentTimeMillis()) {
            error = "配对码已过期"
            return
        }
        scope.launch {
            busy = true
            error = null
            try {
                val result = withContext(Dispatchers.IO) { exchange(payload) }
                val host = SavedHost(
                    macID = result.getString("macID"),
                    macName = result.optString("macName").ifEmpty { "Unpeel" },
                    endpoint = result.optString("directEndpoint").ifEmpty { result.getString("endpoint") },
                    authToken = result.getString("authToken"),
                    fingerprint = result.optString("remoteServerCertificateFingerprint").ifEmpty { payload.certificateFingerprint },
                    relayURL = result.getJSONObject("relayCredentials").getString("relayURL"),
                    relayToken = result.getJSONObject("relayCredentials").getString("relayToken"),
                    e2eKeyB64 = result.getJSONObject("relayCredentials").getString("e2eKeyB64"),
                    serverVersion = result.optString("serverVersion").ifEmpty { null },
                    publicEndpoint = result.optString("publicEndpoint").ifEmpty { null }
                        ?: payload.publicEndpoint,
                )
                hosts = (hosts.filterNot { it.macID == host.macID } + host)
                saveHosts()
                select(host.macID)
            } catch (caught: Exception) {
                error = caught.message ?: "配对失败"
            } finally {
                busy = false
            }
        }
    }

    fun select(macID: String) {
        fitWatchJob?.cancel()
        stopGalleryWatch()
        releasePhoneFit()
        flushSessionCache()
        selectedMacID = macID
        prefs.edit().putString("selected", macID).apply()
        snapshot = null
        openSessionID = null
        startPolling()
    }

    fun forget(macID: String) {
        if (selectedMacID == macID) {
            fitWatchJob?.cancel()
            stopGalleryWatch()
            releasePhoneFit()
            flushSessionCache()
        }
        hosts = hosts.filterNot { it.macID == macID }
        saveHosts()
        if (selectedMacID == macID) {
            selectedMacID = hosts.firstOrNull()?.macID
            prefs.edit().putString("selected", selectedMacID).apply()
            snapshot = null
            openSessionID = null
            if (selectedHost == null) {
                pollJob?.cancel()
                connection = "未配对"
            } else {
                startPolling()
            }
        }
    }

    private fun newTerminal() = HostTerminal(
        replies = { bytes ->
            val id = openSessionID ?: return@HostTerminal
            val text = bytes.toString(Charsets.UTF_8)
            if (text.isEmpty()) return@HostTerminal
            // Keep the keystroke when the link is down. drainWrites sends it
            // on the next link, in order, and does not post into the one closing.
            write(id, text)
        },
        onCopy = { text ->
            val clipboard = appContext.getSystemService(ClipboardManager::class.java) ?: return@HostTerminal
            clipboard.setPrimaryClip(ClipData.newPlainText("unpeel", text))
        },
    )

    private fun rememberSession(id: String): CachedSession {
        sessionCache.firstOrNull { it.id == id }?.let { hit ->
            sessionCache.remove(hit)
            sessionCache.addLast(hit)
            return hit
        }
        val created = CachedSession(id, newTerminal())
        sessionCache.addLast(created)
        while (sessionCache.size > SESSION_CACHE_LIMIT) {
            val victim = sessionCache.firstOrNull { it.id != id } ?: break
            sessionCache.remove(victim)
            victim.terminal.onFrame = null
        }
        return created
    }

    private fun showTerminal(next: HostTerminal) {
        if (terminal === next) return
        terminal.onFrame = null
        terminal = next
    }

    private fun flushSessionCache() {
        outputJob?.cancel()
        outputGeneration += 1
        sessionCache.forEach { it.terminal.onFrame = null }
        sessionCache.clear()
        terminal = newTerminal()
        sessionPreparing = false
        openingFrameReady = false
    }

    fun openSession(id: String) {
        if (openSessionID == id) return
        releasePhoneFit()
        openSessionID = id
        desiredColumns = 0
        desiredRows = 0
        keyboardOpen = false
        lastFitAttemptMs = 0L
        fitRejected = false
        val slot = rememberSession(id)
        showTerminal(slot.terminal)
        val held = slot.outputOffset
        val hasPicture = slot.replayDone && held != null
        val resume = hasPicture && slot.coherent
        // A painted session that lost its stream keeps that frame. The cover
        // is only for a session that has nothing to show yet.
        sessionPreparing = !hasPicture
        openingFrameReady = false
        artifacts = emptyList()
        artifactImage = null
        showGallery = false
        scope.launch { runCatching { link()?.post("mark-read", JSONObject().put("sessionID", id).toString()) } }
        startOutput(
            id,
            resumeOffset = if (resume) held else null,
            keepPicture = hasPicture && !resume,
        )
        startFitWatch(id)
        startGalleryWatch(id)
    }

    fun closeSession() {
        fitWatchJob?.cancel()
        stopGalleryWatch()
        releasePhoneFit()
        outputJob?.cancel()
        openSessionID = null
        sessionPreparing = false
        openingFrameReady = false
        showGallery = false
        artifactImage = null
    }

    /**
     * The measured terminal box. Keyboard height must not change the grid.
     * While this session stays open the phone keeps that grid; the Mac
     * reclaim is its Fit to desktop button, and leaving the session clears it.
     */
    fun notePhoneViewport(columns: Int, rows: Int, keyboardOpen: Boolean) {
        this.keyboardOpen = keyboardOpen
        if (!keyboardOpen && columns > 0 && rows > 0) {
            desiredColumns = columns
            desiredRows = rows
        }
        revealOpening()
    }

    private fun startFitWatch(sessionID: String) {
        fitWatchJob?.cancel()
        fitWatchJob = scope.launch {
            while (openSessionID == sessionID) {
                val metrics = runCatching {
                    JSONObject(link().get("metrics", mapOf("session_id" to sessionID), timeoutMs = 4_000))
                }.getOrNull()
                if (metrics != null && openSessionID == sessionID) {
                    reconcilePhoneFit(
                        sessionID,
                        metrics.optInt("columns", 80),
                        metrics.optInt("rows", 24),
                    )
                }
                delay(1_000)
            }
        }
    }

    private fun reconcilePhoneFit(sessionID: String, observedColumns: Int, observedRows: Int) {
        if (keyboardOpen || desiredColumns < 2 || fitRejected) return
        val ownedColumns = if (phoneFitSession == sessionID) phoneFitColumns else null
        val ownedRows = if (phoneFitSession == sessionID) phoneFitRows else null
        if (ownedColumns == null || ownedRows == null ||
            kotlin.math.abs(desiredColumns - ownedColumns) > 2 ||
            kotlin.math.abs(desiredRows - ownedRows) > 2
        ) {
            lastFitAttemptMs = android.os.SystemClock.elapsedRealtime()
            fitPhone(desiredColumns, desiredRows, force = false)
            return
        }
        // The Mac Fit to desktop button, or any other resize, moved the grid.
        // This session is still open on the phone, so take the phone size back.
        if (kotlin.math.abs(observedColumns - ownedColumns) > 2 ||
            kotlin.math.abs(observedRows - ownedRows) > 2
        ) {
            val now = android.os.SystemClock.elapsedRealtime()
            if (now - lastFitAttemptMs < 3_000) return
            lastFitAttemptMs = now
            fitPhone(desiredColumns, desiredRows, force = true)
        }
    }

    /**
     * Ask the Host to letterbox this session to the phone's terminal grid.
     * The keyboard must not call this. `force` repeats the same grid after
     * the Mac clears it.
     */
    private fun fitPhone(columns: Int, rows: Int, force: Boolean) {
        val sessionID = openSessionID ?: return
        val active = link ?: return
        val cols = columns.coerceIn(20, 300)
        val lines = rows.coerceIn(8, 120)
        if (!force && phoneFitSession == sessionID && phoneFitColumns == cols && phoneFitRows == lines) return
        val generation = phoneFitGeneration
        scope.launch {
            phoneFitLock.withLock {
                if (generation != phoneFitGeneration || openSessionID != sessionID) return@withLock
                val posted = runCatching {
                    active.post(
                        "resize-desktop",
                        JSONObject()
                            .put("sessionID", sessionID)
                            .put("columns", cols)
                            .put("rows", lines)
                            .toString(),
                        8_000,
                    )
                }.isSuccess
                if (generation != phoneFitGeneration || openSessionID != sessionID) {
                    if (posted) revertPhoneFit(active, sessionID)
                    return@withLock
                }
                if (!posted) {
                    fitRejected = true
                    revealOpening()
                    return@withLock
                }
                phoneFitSession = sessionID
                phoneFitColumns = cols
                phoneFitRows = lines
                // Replaying the journal on a row-only change paints the same
                // frame twice and leaves the Grok screen as a short, miscolored
                // block. A new column count has to replay; rows just reflow.
                if (terminal.columns != cols) {
                    startOutput(sessionID, cols, lines)
                } else {
                    terminal.resizeGrid(cols, lines)
                    revealOpening()
                }
            }
        }
    }

    /**
     * Stop polling, the terminal stream, and the fit watch. [revertFit] tells
     * the Host to drop the phone grid. A configuration change passes false:
     * the same process is about to measure the new orientation, and a clear
     * from this instance would race that fit.
     */
    fun close(revertFit: Boolean) {
        pollJob?.cancel()
        outputJob?.cancel()
        outputGeneration += 1
        fitWatchJob?.cancel()
        stopGalleryWatch()
        val revertSession = if (revertFit) phoneFitSession else null
        val revertLink = if (revertFit) link else null
        phoneFitGeneration += 1
        phoneFitSession = null
        phoneFitColumns = null
        phoneFitRows = null
        scope.cancel()
        if (revertSession == null || revertLink == null) return
        CoroutineScope(SupervisorJob() + Dispatchers.IO).launch {
            revertPhoneFit(revertLink, revertSession)
        }
    }

    /** Drop a phone letterbox. Extra clears are safe; the Host treats them as a no-op success. */
    fun releasePhoneFit() {
        val generation = ++phoneFitGeneration
        val sessionID = phoneFitSession
        val active = link
        phoneFitSession = null
        phoneFitColumns = null
        phoneFitRows = null
        if (sessionID == null || active == null) return
        scope.launch {
            phoneFitLock.withLock {
                if (generation != phoneFitGeneration && phoneFitSession == sessionID) return@withLock
                revertPhoneFit(active, sessionID)
            }
        }
    }

    private suspend fun revertPhoneFit(active: HostLink, sessionID: String) {
        runCatching {
            active.post(
                "resize-desktop",
                JSONObject().put("sessionID", sessionID).put("clear", true).toString(),
            )
        }
    }

    fun sendRaw(text: String) {
        val id = openSessionID ?: return
        if (text.isEmpty()) return
        write(id, text)
    }

    /** Upload a JPEG and paste its quoted Host path. Does not submit. */
    fun attachImage(jpeg: ByteArray) {
        uploadAndPaste(jpeg, "image/jpeg", filename = null, failure = "图片上传失败")
    }

    /** Upload any other file and paste its quoted Host path. Does not submit. */
    fun attachFile(bytes: ByteArray, filename: String) {
        uploadAndPaste(bytes, "application/octet-stream", sanitizeUploadName(filename), "文件上传失败")
    }

    private fun uploadAndPaste(bytes: ByteArray, contentType: String, filename: String?, failure: String) {
        val id = openSessionID ?: return
        if (bytes.isEmpty()) {
            error = failure
            return
        }
        val limit = if (filename == null) 4 * 1024 * 1024 else 64 * 1024 * 1024
        if (bytes.size > limit) {
            error = if (filename == null) "图片超过 4 MB" else "文件超过 64 MB"
            return
        }
        scope.launch {
            busy = true
            try {
                val path = uploadChunks(id, bytes, contentType, filename)
                val quoted = "'" + path.replace("'", "'\\''") + "' "
                write(id, TerminalInput.paste(quoted))
            } catch (caught: Exception) {
                error = caught.message ?: failure
            } finally {
                busy = false
            }
        }
    }

    private suspend fun uploadChunks(
        sessionID: String,
        bytes: ByteArray,
        contentType: String,
        filename: String?,
    ): String {
        val uploadID = UUID.randomUUID().toString()
        val digest = sha256Hex(bytes)
        val total = bytes.size.toString()
        val route = if (filename == null) "upload-chunk" else "file-upload-chunk"
        var offset = 0
        var path = ""
        while (offset < bytes.size) {
            val end = minOf(offset + 256 * 1024, bytes.size)
            val query = linkedMapOf(
                "session_id" to sessionID,
                "upload_id" to uploadID,
                "offset" to offset.toString(),
                "total_size" to total,
                "sha256" to digest,
            )
            if (filename != null) query["filename"] = filename
            val body = link().postBinary(route, query, contentType, bytes.copyOfRange(offset, end), 60_000)
            val returned = JSONObject(body).optString("path")
            if (returned.startsWith("/")) path = returned
            offset = end
        }
        if (!path.startsWith("/")) throw HostException(0, "上传没有返回路径")
        return path
    }

    /** Host file names cannot contain a path, `..`, or control characters, and stay within 180 bytes. */
    private fun sanitizeUploadName(raw: String): String {
        val base = raw.substringAfterLast('/').substringAfterLast('\\')
            .filter { !it.isISOControl() }
            .trim()
            .ifEmpty { "file" }
        var flattened = base
        while (".." in flattened) flattened = flattened.replace("..", ".")
        flattened = flattened.trim('.', ' ').ifEmpty { "file" }
        val limited = utf8Prefix(flattened, 180).trim('.', ' ')
        return if (limited.isEmpty() || limited == "." || limited == "..") "file" else limited
    }

    private fun utf8Prefix(value: String, maxBytes: Int): String {
        if (value.toByteArray(Charsets.UTF_8).size <= maxBytes) return value
        var end = value.length
        while (end > 0 && value.substring(0, end).toByteArray(Charsets.UTF_8).size > maxBytes) end -= 1
        return value.substring(0, end)
    }

    fun createSession(projectID: String, presetID: String?, initialText: String) {
        scope.launch {
            busy = true
            try {
                val body = JSONObject().put("projectID", projectID).put("initialTextSubmitMode", "pasteAndSubmit")
                if (!presetID.isNullOrEmpty()) body.put("presetID", presetID)
                if (initialText.isNotEmpty()) body.put("initialText", initialText)
                val response = JSONObject(link().post("sessions", body.toString(), 20_000))
                showCreate = false
                openSession(response.getString("sessionID"))
                refreshOnce()
            } catch (caught: Exception) {
                error = caught.message
            } finally {
                busy = false
            }
        }
    }

    fun answer(id: String, approved: Boolean) {
        scope.launch {
            runCatching {
                link().post("approvals/answer", JSONObject().put("id", id).put("approved", approved).toString())
            }
            refreshOnce()
        }
    }

    fun organize(sessionID: String, title: String? = null, pinned: Boolean? = null) {
        saveSession(sessionID, title, pinned, null) {}
    }

    /** Null fields stay unchanged. `onDone` receives the Host error, or null. */
    fun saveSession(
        sessionID: String,
        title: String?,
        pinned: Boolean?,
        notifyWhenDone: Boolean?,
        onDone: (String?) -> Unit,
    ) {
        scope.launch {
            val body = JSONObject().put("sessionID", sessionID)
            if (title != null) body.put("title", title)
            if (pinned != null) body.put("pinned", pinned)
            if (notifyWhenDone != null) body.put("notifyWhenDone", notifyWhenDone)
            val failure = runCatching { link().post("session-organization", body.toString()) }.exceptionOrNull()
            refreshOnce()
            onDone(failure?.message)
        }
    }

    fun supportsProjectOrganization(): Boolean =
        snapshot?.capabilities?.contains("project.organization.set") == true

    fun saveProject(
        projectID: String,
        displayName: String?,
        colorID: String?,
        dateSorted: Boolean?,
        onDone: (String?) -> Unit,
    ) {
        scope.launch {
            val body = JSONObject().put("projectID", projectID)
            if (displayName != null) body.put("displayName", displayName)
            if (colorID != null) body.put("colorID", colorID)
            if (dateSorted != null) body.put("dateSorted", dateSorted)
            val failure = runCatching { link().post("project-organization", body.toString()) }.exceptionOrNull()
            refreshOnce()
            onDone(failure?.message)
        }
    }

    fun copyTranscript(sessionID: String, entries: Int, onResult: (String?, String?) -> Unit) {
        scope.launch {
            busy = true
            try {
                val body = link().get(
                    "transcript-markdown",
                    mapOf("session_id" to sessionID, "entries" to entries.toString()),
                    timeoutMs = 20_000,
                )
                val markdown = JSONObject(body).optString("markdown").trim()
                if (markdown.isEmpty()) onResult(null, "这个会话还没有可读的记录")
                else onResult(markdown, null)
            } catch (caught: Exception) {
                onResult(null, caught.message ?: "复制失败")
            } finally {
                busy = false
            }
        }
    }

    fun sessionAction(sessionID: String, action: String) {
        scope.launch {
            if (action == "restart" || action == "resume_agent") restartingSessionID = sessionID
            val failure = runCatching {
                link().post(
                    "session-action",
                    JSONObject().put("sessionID", sessionID).put("action", action).toString(),
                )
            }.exceptionOrNull()
            if (failure == null && action == "remove" && openSessionID == sessionID) closeSession()
            if (failure != null) error = failure.message
            if (restartingSessionID == sessionID) restartingSessionID = null
            refreshOnce()
        }
    }

    fun selectWorkspace(id: String) {
        scope.launch {
            runCatching { link().post("select-workspace", JSONObject().put("workspaceId", id).toString()) }
            refreshOnce()
        }
    }

    fun requestScreenshot() {
        val id = openSessionID ?: return
        scope.launch {
            runCatching { link().post("request-screenshot", JSONObject().put("sessionID", id).toString(), 20_000) }
            loadArtifacts()
        }
    }

    fun loadArtifacts() {
        val id = openSessionID ?: return
        galleryPulse = false
        scope.launch {
            artifacts = runCatching { BootstrapJson.artifacts(link().get("artifacts", mapOf("session_id" to id))) }
                .getOrDefault(emptyList())
            showGallery = true
        }
    }

    /** Re-upload an existing capture into the terminal. Does not submit. */
    fun pasteArtifact(kind: String, name: String) {
        val id = openSessionID ?: return
        scope.launch {
            busy = true
            try {
                val bytes = withContext(Dispatchers.IO) { fetchArtifact(id, kind, name) }
                val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
                showGallery = false
                if (bitmap == null) {
                    attachFile(bytes, name)
                } else {
                    val jpeg = java.io.ByteArrayOutputStream()
                    bitmap.compress(android.graphics.Bitmap.CompressFormat.JPEG, 90, jpeg)
                    bitmap.recycle()
                    attachImage(jpeg.toByteArray())
                }
            } catch (caught: Exception) {
                error = caught.message ?: "贴上失败"
            } finally {
                busy = false
            }
        }
    }

    fun openArtifact(kind: String, name: String) {
        val id = openSessionID ?: return
        scope.launch {
            busy = true
            try {
                val bytes = withContext(Dispatchers.IO) { fetchArtifact(id, kind, name) }
                artifactImage = BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
            } catch (caught: Exception) {
                error = caught.message
            } finally {
                busy = false
            }
        }
    }

    fun reportError(message: String) {
        error = message
    }

    fun dismissArtifact() {
        artifactImage = null
    }

    fun dismissError() {
        error = null
    }

    private fun stopGalleryWatch() {
        galleryWatchJob?.cancel()
        galleryWatchJob = null
        galleryBaseline = null
        galleryPulse = false
    }

    private fun startGalleryWatch(sessionID: String) {
        stopGalleryWatch()
        galleryWatchJob = scope.launch {
            while (true) {
                val session = snapshot?.sessions?.firstOrNull { it.id == sessionID }
                if (session != null && AgentCatalog.isAgent(session.providerID, session.command)) {
                    val list = runCatching {
                        BootstrapJson.artifacts(
                            link().get("artifacts", mapOf("session_id" to sessionID), timeoutMs = 8_000),
                        )
                    }.getOrNull()
                    if (list != null) {
                        val newest = list
                            .filter { it.kind == "screenshots" || it.kind == "computer" }
                            .maxOfOrNull { it.modifiedAtUnixMs } ?: 0L
                        val floor = galleryBaseline
                        if (floor == null) {
                            galleryBaseline = newest
                        } else if (newest > floor) {
                            galleryBaseline = newest
                            galleryPulse = true
                            delay(2_200)
                            galleryPulse = false
                        }
                    }
                }
                delay(5_000)
            }
        }
    }

    private fun write(sessionID: String, data: String) {
        if (data.isEmpty()) return
        synchronized(writeLock) {
            val tail = writeQueue.lastOrNull()
            if (tail != null && tail.sessionID == sessionID) {
                tail.data.append(data)
            } else {
                writeQueue.addLast(WriteBatch(sessionID, StringBuilder(data)))
            }
        }
        kickWrites()
    }

    /** Send queued input once a link exists. A missing link leaves the queue in order. */
    private fun kickWrites() {
        val start = synchronized(writeLock) {
            if (writeDraining || writeQueue.isEmpty() || link == null) {
                false
            } else {
                writeDraining = true
                true
            }
        }
        if (start) scope.launch { drainWrites() }
    }

    private fun setLink(next: HostLink?) {
        link = next
        if (next != null) kickWrites()
    }

    private suspend fun drainWrites() {
        while (true) {
            val batch = synchronized(writeLock) {
                val active = link
                if (active == null) {
                    writeDraining = false
                    null
                } else {
                    val next = writeQueue.removeFirstOrNull()
                    if (next == null) {
                        writeDraining = false
                        null
                    } else {
                        active to next
                    }
                }
            } ?: return
            postWrite(batch.first, batch.second.sessionID, batch.second.data.toString())
        }
    }

    private suspend fun postWrite(active: HostLink, sessionID: String, data: String): Boolean {
        return runCatching {
            active.post(
                "write",
                JSONObject()
                    .put("sessionID", sessionID)
                    .put("data", data)
                    .put("wid", UUID.randomUUID().toString())
                    .toString(),
            )
        }.onFailure { error = it.message }.isSuccess
    }

    private fun startPolling() {
        pollJob?.cancel()
        outputJob?.cancel()
        setLink(null)
        outputDial = null
        connection = "正在连接"
        pollJob = scope.launch {
            while (true) {
                refreshOnce()
                delay(if (usingPublic) 1_000 else 2_000)
            }
        }
    }

    private suspend fun refreshOnce() {
        val host = selectedHost ?: return
        try {
            if (usingPublic && !host.publicEndpoint.isNullOrEmpty() && lanAnswers(host)) {
                setLink(null)
                usingPublic = false
                dialEndpoint = null
            }
            val body = connectedLink(host).get("bootstrap", timeoutMs = 8_000)
            snapshot = BootstrapJson.parse(body)
            connection = when {
                usingRelay -> "Link"
                usingPublic -> "公网"
                else -> pathLabel(dialEndpoint ?: host.endpoint)
            }
            error = null
            snapshot?.remoteServerCertificateFingerprint?.let { pin ->
                if (host.fingerprint != pin) updateFingerprint(host.macID, pin)
            }
            noteDial()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (caught: Exception) {
            setLink(null)
            usingPublic = false
            dialEndpoint = null
            connection = caught.message ?: "连接中断"
        }
    }

    /** LAN bootstrap succeeded, so the next connection attempt should take it. */
    private suspend fun lanAnswers(host: SavedHost): Boolean {
        return try {
            DirectLink(host.endpoint, host.authToken, host.fingerprint)
                .get("bootstrap", timeoutMs = 1_200)
            true
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            false
        }
    }

    /**
     * A real path change (LAN, public, or Link) subscribes again from the
     * Host's current screen. The same dial coming back does not.
     */
    private fun noteDial() {
        val next = OutputDial(relay = usingRelay, endpoint = dialEndpoint)
        val previous = outputDial
        outputDial = next
        if (previous == null || previous == next) return
        for (cached in sessionCache) cached.coherent = false
        val sessionID = openSessionID ?: return
        val slot = sessionCache.firstOrNull { it.id == sessionID }
        val keep = slot?.replayDone == true && slot.outputOffset != null
        startOutput(sessionID, keepPicture = keep)
    }

    private suspend fun connectedLink(host: SavedHost): HostLink {
        link?.let { return it }
        val lan = DirectLink(host.endpoint, host.authToken, host.fingerprint)
        try {
            lan.get("bootstrap", timeoutMs = if (host.publicEndpoint.isNullOrEmpty()) 4_000 else 1_500)
            usingRelay = false
            usingPublic = false
            dialEndpoint = host.endpoint
            setLink(lan)
            return lan
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            val publicURL = host.publicEndpoint
            if (!publicURL.isNullOrEmpty() && publicURL != host.endpoint) {
                val pub = DirectLink(publicURL, host.authToken, host.fingerprint)
                try {
                    pub.get("bootstrap", timeoutMs = 4_000)
                    usingRelay = false
                    usingPublic = true
                    dialEndpoint = publicURL
                    setLink(pub)
                    return pub
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    // The public address missed too. Link remains the last dial.
                }
            }
        }
        usingPublic = false
        dialEndpoint = null
        usingRelay = true
        return RelayLink(
            relayUrl = host.relayURL,
            macID = host.macID,
            relayToken = host.relayToken,
            e2eKey = b64d(host.e2eKeyB64),
            deviceID = deviceId,
            authToken = host.authToken,
        ).also { setLink(it) }
    }

    private fun link(): HostLink = link ?: throw HostException(0, "未连接")

    private fun pathLabel(endpoint: String): String {
        val host = runCatching { java.net.URI(endpoint).host }.getOrNull().orEmpty()
        return if (isPrivateHost(host)) "局域网" else "公网"
    }

    private fun isPrivateHost(host: String): Boolean {
        if (host == "localhost" || host.endsWith(".local")) return true
        val parts = host.split('.')
        if (parts.size != 4) return false
        val nums = parts.map { it.toIntOrNull() ?: return false }
        return nums[0] == 10 ||
            (nums[0] == 192 && nums[1] == 168) ||
            (nums[0] == 172 && nums[1] in 16..31) ||
            (nums[0] == 169 && nums[1] == 254) ||
            nums[0] == 127
    }

    private fun noteOpeningFrame(generation: Int) {
        if (generation != outputGeneration || !sessionPreparing) return
        openingFrameReady = true
        if (revealOpening()) return
        val sessionID = openSessionID
        scope.launch {
            delay(2_500)
            if (generation != outputGeneration || openSessionID != sessionID) return@launch
            // The box never reported a grid. Show the frame we have.
            if (sessionPreparing && openingFrameReady && desiredColumns < 2) {
                sessionPreparing = false
            }
        }
    }

    /** The cover stays up while this frame is still the desktop width. */
    private fun revealOpening(): Boolean {
        if (!sessionPreparing || !openingFrameReady) return false
        if (!fitRejected && !phoneGridReady()) return false
        sessionPreparing = false
        return true
    }

    private fun phoneGridReady(): Boolean {
        if (desiredColumns < 2 || desiredRows < 2) return false
        return kotlin.math.abs(terminal.columns - desiredColumns) <= 2 &&
            kotlin.math.abs(terminal.rows - desiredRows) <= 2
    }

    /**
     * Feeds one chunk in a single append. A Grok redraw is one cursor-addressed
     * frame; yielding between slices lets a phone-fit resize land in the middle
     * of it, and the rest of the frame paints on the reflowed screen.
     */
    private fun feedBytes(
        bytes: ByteArray,
        live: Boolean,
        notify: Boolean,
        generation: Int,
        hostSnapshot: Boolean = false,
    ): Boolean {
        if (generation != outputGeneration) return false
        if (bytes.isNotEmpty()) {
            terminal.feed(bytes, live = live, notify = notify, hostSnapshot = hostSnapshot)
        }
        return generation == outputGeneration
    }

    private enum class SocketStream { Stop, Restart, Fallback }

    private data class OutputSocketTarget(val url: String, val fingerprint: String?)

    /**
     * Direct remote-server socket, on a LAN address or a public one.
     * This is not the Link tunnel: the phone dials `remoteServerPort` on the
     * Mac's direct host. If that connect fails, output falls back to GET.
     */
    private fun outputSocketTarget(sessionID: String, offset: Long?): OutputSocketTarget? {
        val host = selectedHost ?: return null
        val port = snapshot?.remoteServerPort ?: return null
        val endpoint = if (usingPublic) {
            dialEndpoint ?: host.publicEndpoint ?: host.endpoint
        } else {
            snapshot?.directEndpoint ?: dialEndpoint ?: host.endpoint
        }
        val pin = snapshot?.remoteServerCertificateFingerprint ?: host.fingerprint
        val url = TerminalOutputFrames.webSocketUrl(
            endpoint = endpoint,
            port = port,
            sessionID = sessionID,
            token = host.authToken,
            offset = offset,
        ) ?: return null
        return OutputSocketTarget(url, pin)
    }

    /**
     * The iOS output socket. A fresh subscribe asks the Host for the current
     * screen, then keeps the journal that follows. A dropped socket returns
     * [SocketStream.Fallback] and the HTTP long-poll takes that cursor.
     */
    private suspend fun streamOutputSocket(
        target: OutputSocketTarget,
        sessionID: String,
        screen: HostTerminal,
        slot: CachedSession?,
        generation: Int,
        queries: TerminalQueryFilter,
        fresh: Boolean,
        onReplaced: () -> Unit,
    ): SocketStream {
        val socket = TerminalOutputSocket(target.fingerprint)
        try {
            if (!socket.open(target.url)) return SocketStream.Fallback
            val helloEvent = withTimeoutOrNull(10_000) { socket.receive() } ?: return SocketStream.Fallback
            val helloText = (helloEvent as? TerminalOutputSocket.Event.Text)?.text ?: return SocketStream.Fallback
            val hello = withContext(Dispatchers.Default) {
                TerminalOutputFrames.parseText(helloText) as? TerminalOutputText.Hello
            } ?: return SocketStream.Fallback
            if (generation != outputGeneration || openSessionID != sessionID) return SocketStream.Stop
            if (hello.sessionId != sessionID) return SocketStream.Fallback
            if (!fresh && hello.cols != null && hello.cols != screen.columns) {
                slot?.outputOffset = null
                slot?.coherent = false
                slot?.replayDone = false
                return SocketStream.Restart
            }
            var pendingReset = fresh || hello.rebased ||
                (slot?.outputOffset != null && hello.startOffset != slot.outputOffset)
            if (pendingReset && !fresh) {
                sessionPreparing = true
                openingFrameReady = false
                slot?.coherent = false
            }
            var expectBaseline = hello.snapshot && pendingReset
            var held = if (pendingReset) null else slot?.outputOffset
            // The Host sends the baseline text before any journal frame. Hold a
            // frame that arrives early so it cannot splice onto the kept screen.
            val beforeBaseline = ArrayDeque<TerminalOutputBinary>()
            while (openSessionID == sessionID && generation == outputGeneration && currentCoroutineContext().isActive) {
                when (val event = socket.receive()) {
                    TerminalOutputSocket.Event.End -> return SocketStream.Fallback
                    is TerminalOutputSocket.Event.Text -> {
                        when (val message = withContext(Dispatchers.Default) { TerminalOutputFrames.parseText(event.text) }) {
                            is TerminalOutputText.Baseline -> {
                                if (!expectBaseline) continue
                                expectBaseline = false
                                if (!applyBaseline(message, screen, slot, generation, queries)) return SocketStream.Stop
                                onReplaced()
                                pendingReset = false
                                held = message.journalOffset
                                for (queued in beforeBaseline) {
                                    val appended = appendOutputFrame(
                                        queued, hello, screen, slot, generation, queries, sessionID,
                                        pendingReset = false,
                                        held,
                                    )
                                    if (appended.stream != null) return appended.stream
                                    held = appended.held
                                }
                                beforeBaseline.clear()
                            }
                            is TerminalOutputText.Error -> Log.i("Unpeel", "output websocket: ${message.message}")
                            else -> Unit
                        }
                    }
                    is TerminalOutputSocket.Event.Binary -> {
                        val frame = withContext(Dispatchers.Default) {
                            TerminalOutputFrames.parseBinary(event.bytes)
                        } ?: return SocketStream.Fallback
                        if (expectBaseline) {
                            if (beforeBaseline.size >= 32) return SocketStream.Fallback
                            beforeBaseline.addLast(frame)
                            continue
                        }
                        val reset = pendingReset
                        val appended = appendOutputFrame(
                            frame, hello, screen, slot, generation, queries, sessionID, pendingReset, held,
                        )
                        if (appended.stream != null) return appended.stream
                        if (reset) onReplaced()
                        pendingReset = false
                        held = appended.held
                    }
                }
            }
            return if (generation != outputGeneration || openSessionID != sessionID) {
                SocketStream.Stop
            } else {
                SocketStream.Fallback
            }
        } catch (cancelled: CancellationException) {
            if (currentCoroutineContext().isActive) return SocketStream.Fallback
            throw cancelled
        } catch (error: Exception) {
            Log.i("Unpeel", "output websocket fell back to http", error)
            return SocketStream.Fallback
        } finally {
            socket.close()
        }
    }

    /**
     * Paint the Host's screen in one frame. Strip off the main thread first.
     * The cursor is the journal offset, not the snapshot's length.
     */
    private suspend fun applyBaseline(
        baseline: TerminalOutputText.Baseline,
        screen: HostTerminal,
        slot: CachedSession?,
        generation: Int,
        queries: TerminalQueryFilter,
    ): Boolean {
        queries.reset()
        val columns = baseline.cols.takeIf { it >= 2 } ?: screen.columns
        val rows = baseline.rows.takeIf { it >= 2 } ?: screen.rows
        val visible = withContext(Dispatchers.Default) { queries.strip(baseline.bytes) }
        if (generation != outputGeneration) return false
        // Silent until the bytes are in hand, then one paint. An empty
        // snapshot still publishes the cleared screen.
        screen.prepare(columns, rows, notify = visible.isEmpty())
        slot?.coherent = false
        if (visible.isNotEmpty() && !feedBytes(
                visible,
                live = false,
                notify = true,
                generation = generation,
                hostSnapshot = true,
            )
        ) {
            return false
        }
        slot?.outputOffset = baseline.journalOffset
        slot?.replayDone = true
        slot?.coherent = true
        noteOpeningFrame(generation)
        return generation == outputGeneration
    }

    private data class FrameAppend(val stream: SocketStream?, val held: Long?)

    private suspend fun appendOutputFrame(
        frame: TerminalOutputBinary,
        hello: TerminalOutputText.Hello,
        screen: HostTerminal,
        slot: CachedSession?,
        generation: Int,
        queries: TerminalQueryFilter,
        sessionID: String,
        pendingReset: Boolean,
        held: Long?,
    ): FrameAppend {
        var payload = frame.payload
        var origin = frame.offset
        if (!pendingReset && held != null) {
            val end = frame.offset + frame.payload.size
            if (end <= held) return FrameAppend(null, held)
            if (frame.offset > held) {
                if (!fillOutputGap(sessionID, held, frame.offset, slot, generation, queries)) {
                    return FrameAppend(SocketStream.Fallback, slot?.outputOffset ?: held)
                }
                origin = frame.offset
            } else {
                val drop = (held - frame.offset).toInt()
                if (drop >= payload.size) return FrameAppend(null, held)
                if (drop > 0) payload = payload.copyOfRange(drop, payload.size)
                origin = held
            }
        }
        if (pendingReset) {
            queries.reset()
            val preamble = hello.modePreamble
            val visible = withContext(Dispatchers.Default) { queries.strip(payload) }
            if (generation != outputGeneration) return FrameAppend(SocketStream.Stop, held)
            val willFeed = (preamble != null && preamble.isNotEmpty()) || visible.isNotEmpty()
            screen.prepare(
                (hello.cols ?: screen.columns).coerceIn(2, 300),
                (hello.rows ?: screen.rows).coerceIn(2, 120),
                notify = !willFeed,
            )
            slot?.coherent = false
            if (preamble != null && preamble.isNotEmpty() && !feedBytes(
                    preamble,
                    live = false,
                    notify = visible.isEmpty(),
                    generation = generation,
                    hostSnapshot = true,
                )
            ) {
                return FrameAppend(SocketStream.Stop, held)
            }
            if (visible.isNotEmpty() && !feedBytes(visible, live = false, notify = true, generation)) {
                return FrameAppend(SocketStream.Stop, held)
            }
            val next = origin + payload.size
            slot?.outputOffset = next
            slot?.replayDone = true
            slot?.coherent = true
            noteOpeningFrame(generation)
            return FrameAppend(null, next)
        }
        val visible = withContext(Dispatchers.Default) { queries.strip(payload) }
        if (generation != outputGeneration) return FrameAppend(SocketStream.Stop, held)
        slot?.coherent = false
        if (visible.isNotEmpty() && !feedBytes(visible, live = true, notify = true, generation)) {
            return FrameAppend(SocketStream.Stop, held)
        }
        val next = origin + payload.size
        slot?.outputOffset = next
        slot?.replayDone = true
        slot?.coherent = true
        noteOpeningFrame(generation)
        return FrameAppend(null, next)
    }

    /** Journal bytes the socket skipped. Read only up to the live frame so it is not painted twice. */
    private suspend fun fillOutputGap(
        sessionID: String,
        from: Long,
        until: Long,
        slot: CachedSession?,
        generation: Int,
        queries: TerminalQueryFilter,
    ): Boolean {
        var cursor = from
        while (cursor < until && generation == outputGeneration) {
            val body = try {
                link().get(
                    "output",
                    mapOf(
                        "session_id" to sessionID,
                        "offset" to cursor.toString(),
                        "limit" to (until - cursor).coerceAtMost(524_288L).toString(),
                        "wait_ms" to "0",
                    ),
                    timeoutMs = 10_000,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                return false
            }
            if (generation != outputGeneration) return false
            val chunk = withContext(Dispatchers.Default) { BootstrapJson.outputChunk(body) }
            if (chunk.truncated || chunk.offset != cursor || chunk.data.isEmpty()) return false
            val take = (until - chunk.offset).toInt().coerceAtMost(chunk.data.size)
            val piece = if (take == chunk.data.size) chunk.data else chunk.data.copyOfRange(0, take.coerceAtLeast(0))
            val visible = withContext(Dispatchers.Default) { queries.strip(piece) }
            if (generation != outputGeneration) return false
            if (visible.isNotEmpty() && !feedBytes(visible, live = true, notify = true, generation)) return false
            cursor = chunk.offset + take
            slot?.outputOffset = cursor
        }
        return cursor == until && generation == outputGeneration
    }

    private fun startOutput(
        sessionID: String,
        columns: Int? = null,
        rows: Int? = null,
        resumeOffset: Long? = null,
        keepPicture: Boolean = false,
    ) {
        outputJob?.cancel()
        val generation = ++outputGeneration
        val slot = sessionCache.firstOrNull { it.id == sessionID }
        val screen = slot?.terminal ?: terminal
        val refit = columns != null && rows != null
        if (columns != null && rows != null) {
            // The cached grid cannot wrap a new column count. Cover the blank reset.
            sessionPreparing = true
            openingFrameReady = false
            screen.prepare(columns, rows)
            slot?.outputOffset = null
            slot?.replayDone = false
            slot?.coherent = false
        } else if (keepPicture) {
            // Leave the last frame up. The snapshot replaces it in one paint.
            sessionPreparing = false
            slot?.coherent = false
        } else if (resumeOffset == null && sessionPreparing) {
            openingFrameReady = false
        }
        val preset = refit
        val resume = resumeOffset != null && !refit && !keepPicture
        outputJob = scope.launch {
            var offset: Long? = if (resume) resumeOffset else null
            var sized = preset || resume || keepPicture
            var replaceWithSnapshot = !resume
            var socketGaveUp = false
            var holdingPicture = keepPicture
            val queries = TerminalQueryFilter()
            while (openSessionID == sessionID && generation == outputGeneration) {
                if (!socketGaveUp) {
                    val target = outputSocketTarget(sessionID, if (replaceWithSnapshot) null else offset)
                    if (target != null) {
                        when (
                            streamOutputSocket(
                                target, sessionID, screen, slot, generation, queries, replaceWithSnapshot,
                            ) { holdingPicture = false }
                        ) {
                            SocketStream.Stop -> return@launch
                            SocketStream.Restart -> {
                                replaceWithSnapshot = true
                                offset = null
                                sized = false
                                continue
                            }
                            SocketStream.Fallback -> {
                                socketGaveUp = true
                                // A kept picture is incoherent with this dial.
                                // Resuming its offset would splice the new tail on.
                                if (!holdingPicture && slot?.replayDone == true) {
                                    replaceWithSnapshot = false
                                    offset = slot.outputOffset ?: offset
                                    sized = true
                                }
                            }
                        }
                        if (generation != outputGeneration || openSessionID != sessionID) return@launch
                    }
                }
                try {
                    val active = link()
                    if (!sized) {
                        val metrics = runCatching {
                            JSONObject(active.get("metrics", mapOf("session_id" to sessionID)))
                        }.getOrNull()
                        if (generation != outputGeneration) return@launch
                        if (metrics != null) {
                            screen.prepare(
                                metrics.optInt("columns", 80).coerceIn(2, 300),
                                metrics.optInt("rows", 24).coerceIn(2, 120),
                            )
                        }
                        sized = true
                    }
                    val query = mutableMapOf(
                        "session_id" to sessionID,
                        "limit" to if (usingRelay) "200000" else "524288",
                        "wait_ms" to if (offset == null) "0" else "8000",
                    )
                    if (offset != null) query["offset"] = offset.toString()
                    val requested = offset
                    val body = active.get("output", query, timeoutMs = if (offset == null) 10_000 else 14_000)
                    if (generation != outputGeneration) return@launch
                    // Base64 of a full-screen redraw is hundreds of kilobytes.
                    // Decode it off the main thread so a wheel can still be drawn.
                    val chunk = withContext(Dispatchers.Default) { BootstrapJson.outputChunk(body) }
                    if (generation != outputGeneration) return@launch
                    // A fresh tail, or a journal that moved under us, is a new
                    // screen. Queries are removed from every chunk: the Mac
                    // terminal already answered them.
                    val replay = requested == null || chunk.truncated || chunk.offset != requested
                    if (replay && requested != null && chunk.modePreamble == null) {
                        // The held offset fell out of the journal. Ask for a fresh tail,
                        // which is the request that carries the mode preamble.
                        sessionPreparing = true
                        openingFrameReady = false
                        slot?.coherent = false
                        offset = null
                        holdingPicture = false
                        continue
                    }
                    if (replay && holdingPicture) {
                        queries.reset()
                        val preambleBytes = chunk.modePreamble
                        val visible = withContext(Dispatchers.Default) { queries.strip(chunk.data) }
                        if (generation != outputGeneration) return@launch
                        val willFeed = (preambleBytes != null && preambleBytes.isNotEmpty()) || visible.isNotEmpty()
                        screen.prepare(screen.columns, screen.rows, notify = !willFeed)
                        slot?.coherent = false
                        if (preambleBytes != null && preambleBytes.isNotEmpty() && !feedBytes(
                                preambleBytes,
                                live = false,
                                notify = visible.isEmpty(),
                                generation = generation,
                                hostSnapshot = true,
                            )
                        ) {
                            return@launch
                        }
                        if (visible.isNotEmpty() && !feedBytes(
                                visible,
                                live = false,
                                notify = true,
                                generation = generation,
                            )
                        ) {
                            return@launch
                        }
                        holdingPicture = false
                        slot?.outputOffset = chunk.nextOffset
                        slot?.replayDone = true
                        slot?.coherent = true
                        noteOpeningFrame(generation)
                        offset = chunk.nextOffset
                        continue
                    }
                    if (replay) {
                        queries.reset()
                        screen.prepare(screen.columns, screen.rows)
                        slot?.replayDone = false
                        slot?.coherent = false
                    }
                    val visible = withContext(Dispatchers.Default) { queries.strip(chunk.data) }
                    if (generation != outputGeneration) return@launch
                    val preamble = if (replay) chunk.modePreamble else null
                    slot?.coherent = false
                    if (preamble != null && !feedBytes(
                            preamble,
                            live = false,
                            notify = visible.isEmpty(),
                            generation = generation,
                            hostSnapshot = true,
                        )
                    ) {
                        return@launch
                    }
                    if (!feedBytes(visible, live = !replay, notify = true, generation)) return@launch
                    slot?.outputOffset = chunk.nextOffset
                    slot?.replayDone = true
                    slot?.coherent = true
                    noteOpeningFrame(generation)
                    offset = chunk.nextOffset
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    if (generation != outputGeneration) return@launch
                    socketGaveUp = false
                    setLink(null)
                    delay(1_000)
                }
            }
        }
    }

    private suspend fun fetchArtifact(sessionID: String, kind: String, name: String): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        var offset = 0L
        var total = Long.MAX_VALUE
        while (out.size() < total) {
            val body = link().get(
                "artifact",
                mapOf(
                    "session_id" to sessionID,
                    "kind" to kind,
                    "name" to name,
                    "offset" to offset.toString(),
                    "max_dim" to "1600",
                ),
                timeoutMs = 20_000,
            )
            val json = JSONObject(body)
            val bytes = b64d(json.optString("dataBase64"))
            if (bytes.isEmpty()) break
            out.write(bytes)
            total = json.optLong("totalSize", out.size().toLong())
            offset = json.optLong("nextOffset", out.size().toLong())
        }
        return out.toByteArray()
    }

    private fun exchange(payload: com.unpeel.controller.PairingPayload): JSONObject {
        val dials = LinkedHashSet<String>()
        dials.add(payload.endpoint)
        payload.publicEndpoint?.let { dials.add(it) }
        var last: java.io.IOException? = null
        for (dial in dials) {
            try {
                return exchangeAt(payload, dial)
            } catch (error: java.io.IOException) {
                last = error
            }
        }
        throw last ?: java.io.IOException("配对失败")
    }

    private fun exchangeAt(payload: com.unpeel.controller.PairingPayload, dial: String): JSONObject {
        val identity = DeviceIdentity(
            id = deviceId,
            name = Build.MODEL ?: "Android",
            platform = "android",
            appVersion = "0.7.7",
        )
        val request = JSONObject()
            .put("token", payload.token)
            .put(
                "device",
                JSONObject()
                    .put("id", identity.id)
                    .put("name", identity.name)
                    .put("platform", identity.platform)
                    .put("appVersion", identity.appVersion),
            )
        val envelope = PairingCrypto.seal(
            request.toString().toByteArray(),
            payload.token,
            payload.macID,
            payload.endpoint,
            PairingCrypto.REQUEST,
        )
        val http = OkHttpClient.Builder()
            .connectTimeout(4, TimeUnit.SECONDS)
            .callTimeout(8, TimeUnit.SECONDS)
            .build()
        val call = http.newCall(
            Request.Builder()
                .url(dial.trimEnd('/') + "/pair")
                .post(envelope.toString().toRequestBody("application/json".toMediaType()))
                .build(),
        )
        call.execute().use { response ->
            val body = response.body?.string().orEmpty()
            if (!response.isSuccessful) {
                val message = runCatching { JSONObject(body).optString("error") }.getOrDefault(body)
                throw HostException(response.code, message.ifEmpty { "配对失败" })
            }
            val plain = PairingCrypto.open(
                JSONObject(body),
                payload.token,
                payload.macID,
                payload.endpoint,
                PairingCrypto.RESPONSE,
            )
            val opened = JSONObject(plain.toString(Charsets.UTF_8))
            acceptPairingResponse(opened, payload, identity.id)
            return opened
        }
    }

    private fun updateFingerprint(macID: String, fingerprint: String) {
        hosts = hosts.map { if (it.macID == macID) it.copy(fingerprint = fingerprint) else it }
        saveHosts()
    }

    private fun loadHosts(): List<SavedHost> {
        val raw = prefs.getString("hosts", null) ?: return emptyList()
        val array = runCatching { JSONArray(raw) }.getOrNull() ?: return emptyList()
        return (0 until array.length()).map { index ->
            val item = array.getJSONObject(index)
            SavedHost(
                macID = item.getString("macID"),
                macName = item.optString("macName"),
                endpoint = item.getString("endpoint"),
                authToken = item.getString("authToken"),
                fingerprint = item.optString("fingerprint").ifEmpty { null },
                relayURL = item.optString("relayURL"),
                relayToken = item.optString("relayToken"),
                e2eKeyB64 = item.optString("e2eKeyB64"),
                serverVersion = item.optString("serverVersion").ifEmpty { null },
                publicEndpoint = item.optString("publicEndpoint").ifEmpty { null },
            )
        }
    }

    private fun saveHosts() {
        val array = JSONArray()
        hosts.forEach { host ->
            array.put(
                JSONObject()
                    .put("macID", host.macID)
                    .put("macName", host.macName)
                    .put("endpoint", host.endpoint)
                    .put("authToken", host.authToken)
                    .put("fingerprint", host.fingerprint ?: JSONObject.NULL)
                    .put("relayURL", host.relayURL)
                    .put("relayToken", host.relayToken)
                    .put("e2eKeyB64", host.e2eKeyB64)
                    .put("serverVersion", host.serverVersion)
                    .put("publicEndpoint", host.publicEndpoint ?: JSONObject.NULL),
            )
        }
        prefs.edit().putString("hosts", array.toString()).apply()
    }
}
