package com.unpeel.android

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/**
 * One `wss://…/api/sessions/<id>/output` connection. The Host pushes the
 * screen and then each new journal page; this does not long-poll.
 */
internal class TerminalOutputSocket(fingerprint: String?) {
    private val client = remoteWebSocketClient(fingerprint)
    private val events = Channel<Event>(Channel.UNLIMITED)
    private val opened = CompletableDeferred<Boolean>()
    private var socket: WebSocket? = null

    suspend fun open(url: String): Boolean {
        socket = client.newWebSocket(Request.Builder().url(url).build(), listener)
        return withTimeoutOrNull(3_000) { opened.await() } == true
    }

    suspend fun receive(): Event = events.receive()

    fun close() {
        socket?.close(1000, "stop")
        socket?.cancel()
        events.trySend(Event.End)
        if (!opened.isCompleted) opened.complete(false)
    }

    private val listener = object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            opened.complete(true)
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            events.trySend(Event.Text(text))
        }

        override fun onMessage(webSocket: WebSocket, bytes: okio.ByteString) {
            events.trySend(Event.Binary(bytes.toByteArray()))
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            webSocket.close(code, reason)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            events.trySend(Event.End)
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            if (!opened.isCompleted) opened.complete(false)
            events.trySend(Event.End)
        }
    }

    sealed class Event {
        data class Text(val text: String) : Event()
        data class Binary(val bytes: ByteArray) : Event()
        data object End : Event()
    }
}
