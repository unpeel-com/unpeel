package com.unpeel.android

import android.content.Context
import android.graphics.BitmapFactory
import android.os.Build
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import com.unpeel.controller.ArtifactSummary
import com.unpeel.controller.BootstrapJson
import com.unpeel.controller.BootstrapSnapshot
import com.unpeel.controller.DeviceIdentity
import com.unpeel.controller.PairingCode
import com.unpeel.controller.PairingCrypto
import com.unpeel.controller.acceptPairingResponse
import com.unpeel.controller.TerminalInput
import com.unpeel.controller.VtScreen
import com.unpeel.controller.b64d
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
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
    private var usingRelay = false

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
    var screen by mutableStateOf(VtScreen(80, 24))
        private set
    var screenRevision by mutableIntStateOf(0)
        private set
    var composer by mutableStateOf("")
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
        selectedMacID = macID
        prefs.edit().putString("selected", macID).apply()
        snapshot = null
        openSessionID = null
        startPolling()
    }

    fun forget(macID: String) {
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
        openSessionID = id
        screen = VtScreen(80, 24)
        screenRevision += 1
        artifacts = emptyList()
        artifactImage = null
        showGallery = false
        scope.launch { runCatching { link()?.post("mark-read", JSONObject().put("sessionID", id).toString()) } }
        startOutput(id)
    }

    fun closeSession() {
        outputJob?.cancel()
        openSessionID = null
        showGallery = false
        artifactImage = null
    }

    fun sendComposer() {
        val text = composer
        val id = openSessionID ?: return
        if (text.isEmpty()) return
        composer = ""
        write(id, TerminalInput.pasteAndSubmit(text))
    }

    fun sendKey(name: String) {
        val id = openSessionID ?: return
        write(id, TerminalInput.key(name))
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
        scope.launch {
            val body = JSONObject().put("sessionID", sessionID)
            if (title != null) body.put("title", title)
            if (pinned != null) body.put("pinned", pinned)
            runCatching { link().post("session-organization", body.toString()) }
            refreshOnce()
        }
    }

    fun sessionAction(sessionID: String, action: String) {
        scope.launch {
            runCatching {
                link().post(
                    "session-action",
                    JSONObject().put("sessionID", sessionID).put("action", action).toString(),
                )
            }
            if (action == "remove" && openSessionID == sessionID) closeSession()
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
        scope.launch {
            artifacts = runCatching { BootstrapJson.artifacts(link().get("artifacts", mapOf("session_id" to id))) }
                .getOrDefault(emptyList())
            showGallery = true
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

    private fun write(sessionID: String, data: String) {
        scope.launch {
            runCatching {
                link().post(
                    "write",
                    JSONObject().put("sessionID", sessionID).put("data", data).put("wid", UUID.randomUUID().toString()).toString(),
                )
            }.onFailure { error = it.message }
        }
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

    private fun startOutput(sessionID: String) {
        outputJob?.cancel()
        outputJob = scope.launch {
            var offset: Long? = null
            var sized = false
            while (openSessionID == sessionID) {
                try {
                    val active = link()
                    if (!sized) {
                        val metrics = runCatching {
                            JSONObject(active.get("metrics", mapOf("session_id" to sessionID)))
                        }.getOrNull()
                        if (metrics != null) {
                            screen.resize(metrics.optInt("columns", 80), metrics.optInt("rows", 24))
                            screenRevision += 1
                        }
                        sized = true
                    }
                    val query = mutableMapOf(
                        "session_id" to sessionID,
                        "limit" to if (usingRelay) "200000" else "524288",
                        "wait_ms" to if (offset == null) "0" else "8000",
                    )
                    if (offset != null) query["offset"] = offset.toString()
                    val chunk = BootstrapJson.outputChunk(
                        active.get("output", query, timeoutMs = if (offset == null) 10_000 else 14_000),
                    )
                    if (offset == null) {
                        screen.reset()
                        chunk.modePreamble?.let { screen.feed(it) }
                    }
                    if (chunk.data.isNotEmpty()) screen.feed(chunk.data)
                    offset = chunk.nextOffset
                    screenRevision += 1
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
