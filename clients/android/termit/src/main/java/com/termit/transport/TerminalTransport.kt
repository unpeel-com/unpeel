package com.termit.transport

/**
 * Byte-stream bridge to a remote executor.
 * Local termMIT never owns a shell/PTY — the peer does.
 */
interface TerminalTransport {
    fun start(listener: Listener)
    fun send(data: ByteArray)
    fun resize(cols: Int, rows: Int)
    fun close()

    interface Listener {
        fun onData(data: ByteArray)
        fun onClosed()
        fun onError(error: Throwable)
    }
}
