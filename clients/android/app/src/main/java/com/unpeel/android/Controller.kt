package com.unpeel.android

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.BitmapFactory
import android.os.Build
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
import com.unpeel.controller.TerminalQueryFilter
import com.unpeel.controller.b64d
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
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
)

class Controller(context: Context) {
    private val appContext = context.applicationContext
    private val prefs = appContext.getSharedPreferences("unpeel", Context.MODE_PRIVATE)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var pollJob: Job? = null
    private var outputJob: Job? = null
    private var link: HostLink? = null
    private var phoneFitGeneration = 0
    private var phoneFitSession: String? = null
    private var phoneFitColumns: Int? = null
    private var phoneFitRows: Int? = null
    private val phoneFitLock = Mutex()
    /** Paste and the Enter that submits it must not be split by another write. */
    private val inputLock = Mutex()
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
    val terminal = HostTerminal(
        replies = { bytes ->
            val id = openSessionID ?: return@HostTerminal
            val active = link ?: return@HostTerminal
            val text = bytes.toString(Charsets.UTF_8)
            if (text.isEmpty()) return@HostTerminal
            scope.launch { postWrite(active, id, text) }
        },
        onCopy = { text ->
            val clipboard = appContext.getSystemService(ClipboardManager::class.java) ?: return@HostTerminal
            clipboard.setPrimaryClip(ClipData.newPlainText("unpeel", text))
        },
    )
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

    fun openSession(id: String) {
        if (openSessionID == id) return
        releasePhoneFit()
        openSessionID = id
        desiredColumns = 0
        desiredRows = 0
        keyboardOpen = false
        lastFitAttemptMs = 0L
        fitRejected = false
        terminal.prepare(80, 24)
        artifacts = emptyList()
        artifactImage = null
        showGallery = false
        scope.launch { runCatching { link()?.post("mark-read", JSONObject().put("sessionID", id).toString()) } }
        startOutput(id)
        startFitWatch(id)
        startGalleryWatch(id)
    }

    fun closeSession() {
        fitWatchJob?.cancel()
        stopGalleryWatch()
        releasePhoneFit()
        outputJob?.cancel()
        openSessionID = null
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
        val cols = columns.coerceIn(2, 300)
        val lines = rows.coerceIn(2, 120)
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
                    return@withLock
                }
                phoneFitSession = sessionID
                phoneFitColumns = cols
                phoneFitRows = lines
                startOutput(sessionID, cols, lines)
            }
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

    fun sendKey(name: String) {
        sendModifiedKey(name, ctrl = false)
    }

    fun sendModifiedKey(name: String, ctrl: Boolean) {
        val id = openSessionID ?: return
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
        if (data.isNotEmpty()) write(id, data)
    }

    fun sendRaw(text: String) {
        val id = openSessionID ?: return
        if (text.isEmpty()) return
        write(id, text)
    }

    /** Upload a JPEG and paste its quoted Host path. Does not submit. */
    fun attachImage(jpeg: ByteArray) {
        val id = openSessionID ?: return
        if (jpeg.isEmpty()) return
        scope.launch {
            busy = true
            try {
                val body = link().postBinary(
                    "upload",
                    mapOf("session_id" to id),
                    "image/jpeg",
                    jpeg,
                    30_000,
                )
                val path = JSONObject(body).optString("path")
                if (path.isEmpty() || !path.startsWith("/")) throw HostException(0, "上传没有返回路径")
                val quoted = "'" + path.replace("'", "'\\''") + "' "
                write(id, TerminalInput.paste(quoted))
            } catch (caught: Exception) {
                error = caught.message ?: "图片上传失败"
            } finally {
                busy = false
            }
        }
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
                    ?: throw HostException(0, "这张图贴不上")
                val jpeg = java.io.ByteArrayOutputStream()
                bitmap.compress(android.graphics.Bitmap.CompressFormat.JPEG, 90, jpeg)
                bitmap.recycle()
                showGallery = false
                attachImage(jpeg.toByteArray())
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
        val active = link ?: return
        scope.launch {
            inputLock.withLock { postWrite(active, sessionID, data) }
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
        link = null
        pollJob = scope.launch {
            while (true) {
                refreshOnce()
                delay(2_000)
            }
        }
    }

    private suspend fun refreshOnce() {
        val host = selectedHost ?: return
        try {
            val body = connectedLink(host).get("bootstrap", timeoutMs = 8_000)
            snapshot = BootstrapJson.parse(body)
            connection = if (usingRelay) "Link" else "局域网"
            error = null
            snapshot?.remoteServerCertificateFingerprint?.let { pin ->
                if (host.fingerprint != pin) updateFingerprint(host.macID, pin)
            }
        } catch (caught: Exception) {
            link = null
            connection = caught.message ?: "连接中断"
        }
    }

    private suspend fun connectedLink(host: SavedHost): HostLink {
        link?.let { return it }
        val direct = DirectLink(host.endpoint, host.authToken, host.fingerprint)
        return try {
            direct.get("bootstrap", timeoutMs = 4_000)
            usingRelay = false
            direct.also { link = it }
        } catch (_: Exception) {
            usingRelay = true
            RelayLink(
                relayUrl = host.relayURL,
                macID = host.macID,
                relayToken = host.relayToken,
                e2eKey = b64d(host.e2eKeyB64),
                deviceID = deviceId,
                authToken = host.authToken,
            ).also { link = it }
        }
    }

    private fun link(): HostLink = link ?: throw HostException(0, "未连接")

    private fun startOutput(sessionID: String, columns: Int? = null, rows: Int? = null) {
        outputJob?.cancel()
        if (columns != null && rows != null) {
            terminal.prepare(columns, rows)
        }
        val preset = columns != null && rows != null
        outputJob = scope.launch {
            var offset: Long? = null
            var sized = preset
            val queries = TerminalQueryFilter()
            while (openSessionID == sessionID) {
                try {
                    val active = link()
                    if (!sized) {
                        val metrics = runCatching {
                            JSONObject(active.get("metrics", mapOf("session_id" to sessionID)))
                        }.getOrNull()
                        if (metrics != null) {
                            terminal.prepare(
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
                    val chunk = BootstrapJson.outputChunk(
                        active.get("output", query, timeoutMs = if (offset == null) 10_000 else 14_000),
                    )
                    // A fresh tail, or a journal that moved under us, is a new
                    // screen. Queries are removed from every chunk: the Mac
                    // terminal already answered them.
                    val replay = requested == null || chunk.truncated || chunk.offset != requested
                    if (replay) {
                        queries.reset()
                        terminal.prepare(terminal.columns, terminal.rows)
                        chunk.modePreamble?.let { terminal.feed(it, live = false) }
                    }
                    val visible = queries.strip(chunk.data)
                    if (visible.isNotEmpty()) terminal.feed(visible, live = !replay)
                    offset = chunk.nextOffset
                } catch (_: Exception) {
                    link = null
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
        val http = OkHttpClient.Builder().callTimeout(15, TimeUnit.SECONDS).build()
        val call = http.newCall(
            Request.Builder()
                .url(payload.endpoint.trimEnd('/') + "/pair")
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
                    .put("serverVersion", host.serverVersion),
            )
        }
        prefs.edit().putString("hosts", array.toString()).apply()
    }
}
