package com.unpeel.controller

import org.json.JSONObject
import java.net.URI
import java.net.URLEncoder

/**
 * Frames on `wss://<host>:<remoteServerPort>/api/sessions/<id>/output`,
 * the same socket iOS uses (`unpeel-host __remote__`).
 *
 * A fresh subscribe adds `snapshot=1`. The Host then sends the current
 * screen as a `baseline` text frame and only the journal bytes after that
 * offset. iOS does not ask for it yet; without the flag the socket is just
 * the raw tail. An explicit `offset` is a resume and never asks again.
 */
object TerminalOutputFrames {
    fun webSocketUrl(
        endpoint: String,
        port: Int,
        sessionID: String,
        token: String,
        offset: Long?,
    ): String? {
        if (sessionID.isEmpty() || token.isEmpty() || port !in 1..65535) return null
        val host = runCatching { URI(endpoint).host }.getOrNull()?.takeIf { it.isNotEmpty() } ?: return null
        val hostPart = if (host.contains(':')) "[$host]" else host
        val query = buildList {
            add("token=${encode(token)}")
            if (offset != null) {
                add("offset=$offset")
            } else {
                add("snapshot=1")
            }
        }.joinToString("&")
        return "wss://$hostPart:$port/api/sessions/${encode(sessionID)}/output?$query"
    }

    fun parseText(text: String): TerminalOutputText {
        val json = runCatching { JSONObject(text) }.getOrNull() ?: return TerminalOutputText.Unknown
        return when (json.optString("type")) {
            "hello" -> {
                val sessionID = json.optString("session_id")
                if (sessionID.isEmpty()) {
                    TerminalOutputText.Unknown
                } else {
                    TerminalOutputText.Hello(
                        sessionId = sessionID,
                        startOffset = json.optLong("start_offset"),
                        outputSize = json.optLong("output_size"),
                        rebased = json.optBoolean("rebased"),
                        cols = optionalInt(json, "cols"),
                        rows = optionalInt(json, "rows"),
                        modePreamble = json.optString("mode_preamble_base64").ifEmpty { null }?.let { encoded ->
                            runCatching { b64d(encoded) }.getOrNull()?.takeIf { it.isNotEmpty() }
                        },
                        snapshot = json.has("snapshot") && !json.isNull("snapshot") && json.optBoolean("snapshot"),
                    )
                }
            }
            "baseline" -> {
                val bytes = json.optString("bytes_base64").ifEmpty { null }?.let { encoded ->
                    runCatching { b64d(encoded) }.getOrNull()
                } ?: return TerminalOutputText.Unknown
                TerminalOutputText.Baseline(
                    journalOffset = json.optLong("journal_offset"),
                    cols = json.optInt("cols"),
                    rows = json.optInt("rows"),
                    bytes = bytes,
                )
            }
            "error" -> TerminalOutputText.Error(json.optString("message").ifEmpty { "unknown error" })
            else -> TerminalOutputText.Unknown
        }
    }

    /** 8-byte big-endian journal offset, then raw terminal bytes. */
    fun parseBinary(data: ByteArray): TerminalOutputBinary? {
        if (data.size < 8) return null
        var offset = 0L
        for (index in 0 until 8) {
            offset = (offset shl 8) or (data[index].toLong() and 0xff)
        }
        return TerminalOutputBinary(offset, data.copyOfRange(8, data.size))
    }

    private fun optionalInt(json: JSONObject, name: String): Int? =
        if (json.has(name) && !json.isNull(name)) json.optInt(name) else null

    private fun encode(value: String): String =
        URLEncoder.encode(value, Charsets.UTF_8.name()).replace("+", "%20")
}

sealed class TerminalOutputText {
    data class Hello(
        val sessionId: String,
        val startOffset: Long,
        val outputSize: Long,
        val rebased: Boolean,
        val cols: Int?,
        val rows: Int?,
        val modePreamble: ByteArray?,
        val snapshot: Boolean,
    ) : TerminalOutputText()

    data class Baseline(
        val journalOffset: Long,
        val cols: Int,
        val rows: Int,
        val bytes: ByteArray,
    ) : TerminalOutputText()

    data class Error(val message: String) : TerminalOutputText()

    data object Unknown : TerminalOutputText()
}

data class TerminalOutputBinary(val offset: Long, val payload: ByteArray)
