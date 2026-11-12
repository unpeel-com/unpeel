package com.unpeel.controller

import org.json.JSONObject
import java.security.SecureRandom
import java.util.Base64

/**
 * Compact pairing code:
 * `UNPEEL:<version>:<host>:<port>:<macID>:<token>:<expiresUnixSeconds>[:proxyID]`
 * A code that carries both addresses appends `:<publicHost>:<publicPort>` instead of a proxy id.
 * The JSON form of [PairingPayload] is also accepted.
 */
object PairingCode {
    private const val PREFIX = "UNPEEL"
    private const val PROXY_PREFIX = "/mobile/pairing-proxy/"

    fun decode(raw: String): PairingPayload? {
        val trimmed = raw.trim()
        if (trimmed.isEmpty()) return null
        if (trimmed.startsWith("{")) return decodeJson(trimmed)
        return decodeCompact(trimmed)
    }

    private fun decodeJson(raw: String): PairingPayload? {
        val objectJson = runCatching { JSONObject(raw) }.getOrNull() ?: return null
        val endpoint = objectJson.optString("endpoint")
        val token = objectJson.optString("token")
        val macID = objectJson.optString("macID")
        val expires = objectJson.optLong("expiresAtUnixMs", -1)
        if (endpoint.isEmpty() || token.isEmpty() || macID.isEmpty() || expires < 0) return null
        return PairingPayload(
            protocolVersion = objectJson.optInt("protocolVersion", 1),
            macID = macID,
            macName = objectJson.optString("macName"),
            endpoint = endpoint,
            token = token,
            certificateFingerprint = objectJson.optString("certificateFingerprint").ifEmpty { null },
            expiresAtUnixMs = expires,
            publicEndpoint = objectJson.optString("publicEndpoint").ifEmpty { null },
        )
    }

    private fun decodeCompact(raw: String): PairingPayload? {
        val parts = raw.split(":")
        if (parts.size != 7 && parts.size != 8 && parts.size != 9) return null
        if (!parts[0].equals(PREFIX, ignoreCase = true)) return null
        val version = parts[1].toIntOrNull() ?: return null
        val port = parts[3].toIntOrNull() ?: return null
        if (port !in 1..65535) return null
        val expiresSeconds = parts[6].toLongOrNull() ?: return null
        val host = parts[2]
        val macID = parts[4].lowercase()
        val token = parts[5]
        if (host.isEmpty() || macID.isEmpty() || token.isEmpty()) return null
        val path: String
        val publicEndpoint: String?
        if (parts.size == 9) {
            val publicHost = parts[7]
            val publicPort = parts[8].toIntOrNull() ?: return null
            if (publicHost.isEmpty() || publicHost.contains("/") || publicPort !in 1..65535) return null
            path = "/mobile"
            publicEndpoint = "http://$publicHost:$publicPort/mobile"
        } else if (parts.size == 8) {
            val proxyID = parts[7]
            if (!proxyID.matches(Regex("^[A-Za-z0-9-]+$"))) return null
            path = PROXY_PREFIX + proxyID
            publicEndpoint = null
        } else {
            path = "/mobile"
            publicEndpoint = null
        }
        return PairingPayload(
            protocolVersion = version,
            macID = macID,
            macName = "",
            endpoint = "http://$host:$port$path",
            token = token,
            certificateFingerprint = null,
            expiresAtUnixMs = expiresSeconds * 1000,
            publicEndpoint = publicEndpoint,
        )
    }
}

data class PairingPayload(
    val protocolVersion: Int,
    val macID: String,
    val macName: String,
    val endpoint: String,
    val token: String,
    val certificateFingerprint: String?,
    val expiresAtUnixMs: Long,
    val publicEndpoint: String? = null,
)

data class DeviceIdentity(
    val id: String,
    val name: String,
    val platform: String,
    val appVersion: String?,
)

data class PairingResult(
    val protocolVersion: Int,
    val macID: String,
    val macName: String,
    val endpoint: String,
    val directEndpoint: String?,
    val deviceID: String,
    val authToken: String,
    val pairedAtUnixMs: Long,
    val remoteServerPort: Int?,
    val remoteServerCertificateFingerprint: String?,
    val relayURL: String,
    val relayToken: String,
    val e2eKeyB64: String,
    val serverVersion: String?,
)

class PairingRejected(message: String) : IllegalArgumentException(message)

/**
 * Accept a sealed pairing response only when it names this device, this Host,
 * and a `/mobile` direct route. The checks match the iOS Controller.
 */
fun acceptPairingResponse(json: JSONObject, payload: PairingPayload, deviceID: String): PairingResult {
    if (json.optInt("protocolVersion", -1) != 1) throw PairingRejected("协议版本不兼容")
    val macID = json.optString("macID")
    if (macID != payload.macID) throw PairingRejected("Host 身份不匹配")
    val endpoint = json.optString("endpoint")
    if (!sameResource(endpoint, payload.endpoint)) throw PairingRejected("配对地址不匹配")
    val direct = json.optString("directEndpoint").ifEmpty { null }
    if (direct != null && !validDirectEndpoint(direct)) throw PairingRejected("直连地址无效")
    if (json.optString("deviceID") != deviceID) throw PairingRejected("设备身份不匹配")
    val authToken = json.optString("authToken")
    val relay = json.optJSONObject("relayCredentials") ?: throw PairingRejected("配对凭据无效")
    val relayURL = relay.optString("relayURL")
    val relayToken = relay.optString("relayToken")
    val e2eKeyB64 = relay.optString("e2eKeyB64")
    val e2eKey = runCatching { b64d(e2eKeyB64) }.getOrNull()
    val relayScheme = runCatching { java.net.URI(relayURL).scheme }.getOrNull()?.lowercase()
    if (authToken.isEmpty() ||
        relay.optString("macID") != payload.macID ||
        relayToken.isEmpty() ||
        relayScheme != "wss" ||
        e2eKey == null ||
        e2eKey.size != 32
    ) {
        throw PairingRejected("配对凭据无效")
    }
    return PairingResult(
        protocolVersion = 1,
        macID = macID,
        macName = json.optString("macName"),
        endpoint = endpoint,
        directEndpoint = direct,
        deviceID = deviceID,
        authToken = authToken,
        pairedAtUnixMs = json.optLong("pairedAtUnixMs"),
        remoteServerPort = if (json.has("remoteServerPort") && !json.isNull("remoteServerPort")) json.optInt("remoteServerPort") else null,
        remoteServerCertificateFingerprint = json.optString("remoteServerCertificateFingerprint").ifEmpty { null },
        relayURL = relayURL,
        relayToken = relayToken,
        e2eKeyB64 = e2eKeyB64,
        serverVersion = json.optString("serverVersion").ifEmpty { null },
    )
}

private fun validDirectEndpoint(value: String): Boolean {
    val uri = runCatching { java.net.URI(value) }.getOrNull() ?: return false
    val scheme = uri.scheme?.lowercase()
    return (scheme == "http" || scheme == "https") &&
        !uri.host.isNullOrEmpty() &&
        uri.port in 1..65535 &&
        uri.rawPath == "/mobile" &&
        uri.rawQuery == null &&
        uri.rawFragment == null
}

private fun sameResource(left: String, right: String): Boolean {
    if (left == right) return true
    val a = runCatching { java.net.URI(left) }.getOrNull() ?: return false
    val b = runCatching { java.net.URI(right) }.getOrNull() ?: return false
    return a.scheme.equals(b.scheme, ignoreCase = true) &&
        a.host.equals(b.host, ignoreCase = true) &&
        a.port == b.port &&
        a.rawPath == b.rawPath &&
        a.rawQuery == b.rawQuery &&
        a.rawFragment == b.rawFragment
}

/**
 * Sealed LAN pairing. Each direction has its own HKDF key; associated data
 * binds the box to the scanned Host id and endpoint string.
 */
object PairingCrypto {
    const val REQUEST = "phone-to-mac"
    const val RESPONSE = "mac-to-phone"

    fun seal(plaintext: ByteArray, token: String, macID: String, endpoint: String, direction: String): JSONObject {
        require(token.isNotEmpty() && macID.isNotEmpty())
        val salt = ByteArray(16).also { SecureRandom().nextBytes(it) }
        val key = deriveKey(token, salt, direction)
        val nonce = ByteArray(12).also { SecureRandom().nextBytes(it) }
        val boxed = nonce + AesGcm.seal(key, nonce, plaintext, associatedData(macID, endpoint, direction))
        return JSONObject()
            .put("v", 1)
            .put("saltB64", b64(salt))
            .put("sealedB64", b64(boxed))
    }

    fun open(envelope: JSONObject, token: String, macID: String, endpoint: String, direction: String): ByteArray {
        if (envelope.optInt("v") != 1 || token.isEmpty() || macID.isEmpty()) {
            throw IllegalArgumentException("invalid pairing envelope")
        }
        val salt = b64d(envelope.getString("saltB64"))
        val sealed = b64d(envelope.getString("sealedB64"))
        if (salt.size != 16 || sealed.size < 12 + 16) throw IllegalArgumentException("invalid pairing envelope")
        val nonce = sealed.copyOfRange(0, 12)
        val body = sealed.copyOfRange(12, sealed.size)
        return AesGcm.open(deriveKey(token, salt, direction), nonce, body, associatedData(macID, endpoint, direction))
    }

    private fun deriveKey(token: String, salt: ByteArray, direction: String): ByteArray =
        Hkdf.sha256(token.toByteArray(Charsets.UTF_8), salt, "unpeel-pairing-v1:$direction".toByteArray(Charsets.UTF_8))

    private fun associatedData(macID: String, endpoint: String, direction: String): ByteArray =
        "unpeel-pairing-v1\u0000$direction\u0000$macID\u0000$endpoint".toByteArray(Charsets.UTF_8)
}

fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

fun b64d(text: String): ByteArray = Base64.getDecoder().decode(text)
