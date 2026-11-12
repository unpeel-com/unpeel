package com.unpeel.controller

/**
 * The slice of iOS `RemoteTerminalMouseModeTracker` that wheel forwarding
 * needs. A host mode snapshot is authoritative. Until one arrives, a guessed
 * mouse-wheel program keeps forwarding unless this stream turns mouse
 * tracking or the alternate screen off, or resets the terminal.
 */
class TerminalModeSignals {
    var hostModeSnapshot: Boolean = false
        private set

    var sawMouseOrAlternateDisable: Boolean = false
        private set

    private var pending = ByteArray(0)

    fun reset() {
        hostModeSnapshot = false
        sawMouseOrAlternateDisable = false
        pending = ByteArray(0)
    }

    fun markHostSnapshot() {
        hostModeSnapshot = true
    }

    fun feed(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        val data = if (pending.isEmpty()) bytes else pending + bytes
        pending = ByteArray(0)
        var index = 0
        while (index < data.size) {
            if (data[index] != ESC) {
                index++
                continue
            }
            if (index + 1 >= data.size) {
                carry(data, index)
                break
            }
            if (data[index + 1] == RIS) {
                sawMouseOrAlternateDisable = true
                index += 2
                continue
            }
            if (data[index + 1] != CSI) {
                index += 2
                continue
            }
            if (index + 2 >= data.size) {
                carry(data, index)
                break
            }
            var cursor = index + 2
            val privateMode = data[cursor] == QUESTION
            if (privateMode) cursor++
            val paramsStart = cursor
            while (cursor < data.size && !isFinal(data[cursor])) cursor++
            if (cursor >= data.size) {
                carry(data, index)
                break
            }
            val finalByte = data[cursor]
            if (privateMode && (finalByte == SET || finalByte == RESET)) {
                noteParams(data, paramsStart, cursor, enabled = finalByte == SET)
            }
            index = cursor + 1
        }
    }

    private fun carry(data: ByteArray, from: Int) {
        val length = data.size - from
        pending = if (length > MAX_PENDING) ByteArray(0) else data.copyOfRange(from, data.size)
    }

    private fun noteParams(data: ByteArray, start: Int, end: Int, enabled: Boolean) {
        var value = 0
        var inNumber = false
        for (index in start until end) {
            val byte = data[index]
            if (byte in ZERO..NINE) {
                value = value * 10 + (byte - ZERO)
                inNumber = true
            } else if (byte == SEMICOLON) {
                if (inNumber) note(value, enabled)
                value = 0
                inNumber = false
            }
        }
        if (inNumber) note(value, enabled)
    }

    private fun note(mode: Int, enabled: Boolean) {
        if (mode == 9 || mode == 47 || mode == 1000 || mode == 1002 || mode == 1003 ||
            mode == 1047 || mode == 1049
        ) {
            sawMouseOrAlternateDisable = !enabled
        }
    }

    private fun isFinal(byte: Byte): Boolean {
        val value = byte.toInt() and 0xFF
        return value in 0x40..0x7E
    }

    private companion object {
        const val MAX_PENDING = 96
        const val ESC = 0x1B.toByte()
        const val RIS = 'c'.code.toByte()
        const val CSI = '['.code.toByte()
        const val QUESTION = '?'.code.toByte()
        const val SET = 'h'.code.toByte()
        const val RESET = 'l'.code.toByte()
        const val SEMICOLON = ';'.code.toByte()
        const val ZERO = '0'.code.toByte()
        const val NINE = '9'.code.toByte()
    }
}
