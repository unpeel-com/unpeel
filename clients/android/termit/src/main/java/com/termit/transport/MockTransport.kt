package com.termit.transport

import java.nio.charset.StandardCharsets
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Local echo + tiny command stubs for UI development without a remote peer.
 */
class MockTransport(
    private val banner: String = "termMIT mock remote — type help\r\n$ ",
) : TerminalTransport {
    private val running = AtomicBoolean(false)
    private val executor = Executors.newSingleThreadExecutor()
    private var listener: TerminalTransport.Listener? = null
    private val line = StringBuilder()
    private var cols = 80
    private var rows = 24

    override fun start(listener: TerminalTransport.Listener) {
        this.listener = listener
        running.set(true)
        emit(banner)
    }

    override fun send(data: ByteArray) {
        if (!running.get()) return
        val text = String(data, StandardCharsets.UTF_8)
        for (ch in text) {
            when (ch) {
                '\r', '\n' -> {
                    emit("\r\n")
                    handleCommand(line.toString())
                    line.clear()
                    emit("$ ")
                }
                '\u007f', '\b' -> {
                    if (line.isNotEmpty()) {
                        line.deleteCharAt(line.length - 1)
                        emit("\b \b")
                    }
                }
                else -> {
                    if (ch.code >= 32) {
                        line.append(ch)
                        emit(ch.toString())
                    }
                }
            }
        }
    }

    override fun resize(cols: Int, rows: Int) {
        this.cols = cols.coerceAtLeast(1)
        this.rows = rows.coerceAtLeast(1)
    }

    override fun close() {
        if (!running.getAndSet(false)) return
        executor.shutdownNow()
        listener?.onClosed()
        listener = null
    }

    private fun handleCommand(cmd: String) {
        when (cmd.trim()) {
            "" -> Unit
            "help" -> emit("commands: help, clear, size, echo <text>\r\n")
            "clear" -> emit("\u001b[2J\u001b[H")
            "size" -> emit("${cols}x${rows}\r\n")
            else -> if (cmd.startsWith("echo ")) {
                emit(cmd.removePrefix("echo ") + "\r\n")
            } else {
                emit("unknown: $cmd\r\n")
            }
        }
    }

    private fun emit(text: String) {
        val bytes = text.toByteArray(StandardCharsets.UTF_8)
        executor.execute {
            if (running.get()) listener?.onData(bytes)
        }
    }
}
