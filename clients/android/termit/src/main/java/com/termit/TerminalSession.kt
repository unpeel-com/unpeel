package com.termit

import com.termit.transport.TerminalTransport
import com.termit.vt.VtEmulator
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Binds a [TerminalTransport] to a [VtEmulator].
 * Thread-safe for transport callbacks; UI should observe via [Listener].
 */
class TerminalSession(
    private val transport: TerminalTransport,
    cols: Int = 80,
    rows: Int = 24,
) {
    val emulator = VtEmulator(cols, rows)
    private val listeners = CopyOnWriteArrayList<Listener>()
    private val started = AtomicBoolean(false)
    private val lock = Any()

    fun addListener(listener: Listener) {
        listeners += listener
    }

    fun removeListener(listener: Listener) {
        listeners -= listener
    }

    fun start() {
        if (!started.compareAndSet(false, true)) return
        transport.start(object : TerminalTransport.Listener {
            override fun onData(data: ByteArray) {
                synchronized(lock) {
                    emulator.feed(data)
                }
                listeners.forEach { it.onScreenUpdate() }
            }

            override fun onClosed() {
                listeners.forEach { it.onClosed() }
            }

            override fun onError(error: Throwable) {
                listeners.forEach { it.onError(error) }
            }
        })
    }

    fun write(data: ByteArray) = transport.send(data)

    fun write(text: String) = write(text.toByteArray(Charsets.UTF_8))

    fun resize(cols: Int, rows: Int) {
        synchronized(lock) {
            emulator.resize(cols, rows)
        }
        transport.resize(cols, rows)
        listeners.forEach { it.onScreenUpdate() }
    }

    fun close() {
        transport.close()
        listeners.clear()
    }

    interface Listener {
        fun onScreenUpdate()
        fun onClosed() {}
        fun onError(error: Throwable) {}
    }
}
