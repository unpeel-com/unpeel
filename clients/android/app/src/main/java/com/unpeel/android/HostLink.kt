package com.unpeel.android

import com.unpeel.controller.RelayHandshake
import com.unpeel.controller.RelaySession
import com.unpeel.controller.b64
import com.unpeel.controller.b64d
import com.unpeel.controller.constantTimeEquals
import com.unpeel.controller.sha256Hex
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.json.JSONObject
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

class HostException(val status: Int, message: String) : Exception(message.ifEmpty { "HTTP $status" })

interface HostLink {
    suspend fun get(path: String, query: Map<String, String> = emptyMap(), timeoutMs: Long = 10_000): String
    suspend fun post(path: String, json: String, timeoutMs: Long = 10_000): String
    suspend fun postBinary(
        path: String,
        query: Map<String, String>,
        contentType: String,
        bytes: ByteArray,
        timeoutMs: Long = 30_000,
    ): String
}

/** Direct `/mobile` on the LAN. A certificate pin rewrites the URL to HTTPS. */
class DirectLink(
    baseUrl: String,
    private val authToken: String,
    fingerprint: String?,
) : HostLink {
    private val root = if (fingerprint != null) {
        baseUrl.replaceFirst("http://", "https://")
    } else {
        baseUrl
    }.trimEnd('/')
    private val client = httpClient(fingerprint)

    override suspend fun get(path: String, query: Map<String, String>, timeoutMs: Long): String =
        call("GET", path, query, null, null, timeoutMs)

    override suspend fun post(path: String, json: String, timeoutMs: Long): String =
        call("POST", path, emptyMap(), json.toByteArray(), "application/json", timeoutMs)

    override suspend fun postBinary(
        path: String,
        query: Map<String, String>,
        contentType: String,
        bytes: ByteArray,
        timeoutMs: Long,
    ): String = call("POST", path, query, bytes, contentType, timeoutMs)

    private suspend fun call(
        method: String,
        path: String,
        query: Map<String, String>,
        body: ByteArray?,
        contentType: String?,
        timeoutMs: Long,
    ): String = withContext(Dispatchers.IO) {
        val url = buildString {
            append(root).append('/').append(path.trimStart('/'))
            if (query.isNotEmpty()) {
                append('?')
                append(query.toSortedMap().entries.joinToString("&") { (key, value) ->
                    "${encode(key)}=${encode(value)}"
                })
            }
        }
        val builder = Request.Builder().url(url).header("Authorization", "Bearer $authToken")
        if (method == "POST") {
            val type = (contentType ?: "application/json").toMediaType()
            builder.post((body ?: "{}".toByteArray()).toRequestBody(type))
        }
        val call = client.newCall(builder.build())
        call.timeout().timeout(timeoutMs, TimeUnit.MILLISECONDS)
        call.execute().use { response -> read(response) }
    }

    private fun read(response: Response): String {
        val body = response.body?.string().orEmpty()
        if (!response.isSuccessful) {
            val message = runCatching { JSONObject(body).optString("error") }.getOrDefault("")
            throw HostException(response.code, message.ifEmpty { body })
        }
        return body
    }

    private fun encode(value: String) = java.net.URLEncoder.encode(value, Charsets.UTF_8.name())
}

/**
 * Link relay. The WebSocket carries a plaintext handshake, then sealed
 * `/mobile` requests. The relay token is a subprotocol, not a query string.
 */
class RelayLink(
    private val relayUrl: String,
    private val macID: String,
    private val relayToken: String,
    private val e2eKey: ByteArray,
    private val deviceID: String,
    private val authToken: String,
) : HostLink {
    private val http = OkHttpClient.Builder()
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .pingInterval(15, TimeUnit.SECONDS)
        .build()
    private val mutex = Mutex()
    private var socket: WebSocket? = null
    private var session: RelaySession? = null
    private var nextId = 0L
    private val pending = ConcurrentHashMap<Long, CompletableDeferred<JSONObject>>()

    override suspend fun get(path: String, query: Map<String, String>, timeoutMs: Long): String =
        tunnel("GET", path, query, null, null, timeoutMs)

    override suspend fun post(path: String, json: String, timeoutMs: Long): String =
        tunnel("POST", path, emptyMap(), json.toByteArray(), "application/json", timeoutMs)

    override suspend fun postBinary(
        path: String,
        query: Map<String, String>,
        contentType: String,
        bytes: ByteArray,
        timeoutMs: Long,
    ): String = tunnel("POST", path, query, bytes, contentType, timeoutMs)

    private suspend fun tunnel(
        method: String,
        path: String,
        query: Map<String, String>,
        body: ByteArray?,
        contentType: String?,
        timeoutMs: Long,
    ): String {
        val id = mutex.withLock {
            ensureConnected()
            ++nextId
        }
        val deferred = CompletableDeferred<JSONObject>()
        pending[id] = deferred
        val request = JSONObject()
            .put("id", id)
            .put("method", method)
            .put("path", "/mobile/${path.trimStart('/')}")
            .put("query", JSONObject(query))
            .put("auth", "Bearer $authToken")
        if (body != null) {
            request.put("contentType", contentType ?: "application/json")
            request.put("bodyB64", b64(body))
        }
        val sealed = mutex.withLock {
            val current = session ?: throw HostException(0, "relay closed")
            synchronized(current) { current.seal(request.toString().toByteArray()) }
        }
        val sent = socket?.send(sealed.toByteString()) == true
        if (!sent) {
            pending.remove(id)
            drop()
            throw HostException(0, "relay send failed")
        }
        val response = try {
            withTimeout(timeoutMs) { deferred.await() }
        } catch (error: Exception) {
            pending.remove(id)
            throw error
        }
        val status = response.optInt("status")
        val body = response.optString("bodyB64").let { encoded ->
            if (encoded.isEmpty()) "" else b64d(encoded).toString(Charsets.UTF_8)
        }
        if (status !in 200..299) throw HostException(status, runCatching { JSONObject(body).optString("error") }.getOrDefault(body))
        return body
    }

    private suspend fun ensureConnected() {
        if (socket != null && session != null) return
        val helloWait = CompletableDeferred<ByteArray>()
        val opened = CompletableDeferred<Unit>()
        val url = relayUrl.trimEnd('/') + "/v1/client/$macID"
        val request = Request.Builder()
            .url(url)
            .header("Sec-WebSocket-Protocol", "unpeel-relay, unpeel-relay-token.$relayToken")
            .build()
        val webSocket = http.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                opened.complete(Unit)
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                accept(bytes.toByteArray(), helloWait)
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                accept(text.toByteArray(), helloWait)
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                if (!helloWait.isCompleted) helloWait.completeExceptionally(t)
                if (!opened.isCompleted) opened.completeExceptionally(t)
                failAll(t)
                clear(webSocket)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                failAll(HostException(code, reason))
                clear(webSocket)
            }
        })
        socket = webSocket
        try {
            withTimeout(10_000) { opened.await() }
            val salt = ByteArray(16).also { SecureRandom().nextBytes(it) }
            val ephemeral = RelayHandshake.generateEphemeral()
            val hello = JSONObject()
                .put("v", 1)
                .put("deviceID", deviceID)
                .put("saltB64", b64(salt))
                .put("ephemeralPublicKeyB64", b64(ephemeral.publicKey))
            webSocket.send(hello.toString().toByteArray().toByteString())
            val reply = JSONObject(withTimeout(10_000) { helloWait.await() }.toString(Charsets.UTF_8))
            val hostSalt = b64d(reply.getString("saltB64"))
            val hostPublic = b64d(reply.getString("ephemeralPublicKeyB64"))
            val hostMac = b64d(reply.getString("macB64"))
            val expected = RelayHandshake.transcriptMac(
                e2eKey, deviceID, salt, hostSalt, ephemeral.publicKey, hostPublic,
            )
            if (reply.optInt("v") != 1 || !constantTimeEquals(hostMac, expected)) {
                throw HostException(0, "relay handshake rejected")
            }
            val secret = RelayHandshake.sharedSecret(ephemeral.privateKey, hostPublic)
            session = RelaySession(e2eKey, secret, salt, hostSalt, isHost = false)
        } catch (error: Exception) {
            webSocket.close(1002, "handshake")
            socket = null
            session = null
            throw error
        }
    }

    private fun accept(bytes: ByteArray, helloWait: CompletableDeferred<ByteArray>) {
        val current = session
        if (current == null) {
            if (!helloWait.isCompleted) helloWait.complete(bytes)
            return
        }
        val plain = try {
            synchronized(current) { current.open(bytes) }
        } catch (error: Exception) {
            failAll(error)
            socket?.close(1002, "aead")
            return
        }
        val json = runCatching { JSONObject(plain.toString(Charsets.UTF_8)) }.getOrNull() ?: return
        if (!json.has("status")) return
        pending.remove(json.optLong("id"))?.complete(json)
    }

    private fun failAll(error: Throwable) {
        pending.values.forEach { it.completeExceptionally(error) }
        pending.clear()
    }

    private fun clear(webSocket: WebSocket) {
        if (socket === webSocket) {
            socket = null
            session = null
        }
    }

    private fun drop() {
        socket?.cancel()
        socket = null
        session = null
    }
}

/** WebSocket client for `unpeel-host __remote__`. Same leaf pin as `/mobile`. */
internal fun remoteWebSocketClient(fingerprint: String?): OkHttpClient =
    httpClient(fingerprint).newBuilder()
        .callTimeout(0, TimeUnit.MILLISECONDS)
        .connectTimeout(3, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .writeTimeout(0, TimeUnit.MILLISECONDS)
        .pingInterval(15, TimeUnit.SECONDS)
        .build()

private fun httpClient(fingerprint: String?): OkHttpClient {
    val builder = OkHttpClient.Builder()
        .callTimeout(30, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
    if (fingerprint != null) {
        val trust = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) = Unit

            override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                val leaf = chain?.firstOrNull() ?: throw CertificateException("empty chain")
                val actual = sha256Hex(leaf.encoded)
                if (!actual.equals(fingerprint, ignoreCase = true)) {
                    throw CertificateException("certificate pin mismatch")
                }
            }

            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val context = SSLContext.getInstance("TLS")
        context.init(null, arrayOf<TrustManager>(trust), SecureRandom())
        builder.sslSocketFactory(context.socketFactory, trust)
        builder.hostnameVerifier { _, _ -> true }
    }
    return builder.build()
}
