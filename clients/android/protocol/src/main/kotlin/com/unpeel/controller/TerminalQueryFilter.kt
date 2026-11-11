package com.unpeel.controller

/**
 * Strips terminal query requests before they reach the phone's local emulator.
 *
 * The Mac's terminal already answered these. A second answer from the phone
 * is written back as input. Same sequences and split-chunk rules as the iOS
 * filter: CSI DA, DSR, XTVERSION, DECRQM, and DCS XTGETTCAP / DECRQSS.
 * RIS (`ESC c`) and DECSCUSR stay.
 */
class TerminalQueryFilter {
    private val carry = ArrayList<Byte>(16)
    private var discardingDCSQuery = false

    fun reset() {
        carry.clear()
        discardingDCSQuery = false
    }

    fun strip(input: ByteArray): ByteArray {
        if (carry.isEmpty() && !discardingDCSQuery && input.none { it.u() == 0x1B }) return input
        val bytes = ByteArray(carry.size + input.size)
        var position = 0
        for (held in carry) bytes[position++] = held
        input.copyInto(bytes, position)
        carry.clear()
        val n = bytes.size
        val out = ArrayList<Byte>(n)
        var i = 0
        if (discardingDCSQuery) {
            when (val scan = scanDCSTerminator(bytes, 0)) {
                is DCS.Terminated -> {
                    discardingDCSQuery = false
                    i = scan.end
                }
                DCS.TrailingEsc -> {
                    carry.add(0x1B.toByte())
                    return ByteArray(0)
                }
                DCS.Exhausted -> return ByteArray(0)
            }
        }
        while (i < n) {
            if (bytes[i].u() != 0x1B) {
                out.add(bytes[i])
                i++
                continue
            }
            if (i + 1 >= n) {
                carry.add(0x1B.toByte())
                break
            }
            val next = bytes[i + 1].u()
            if (next == 0x5B) {
                var j = i + 2
                var priv: Int? = null
                if (j < n && bytes[j].u() in 0x3C..0x3F) {
                    priv = bytes[j].u()
                    j++
                }
                var hasDollar = false
                while (j < n && bytes[j].u() !in 0x40..0x7E) {
                    if (bytes[j].u() == 0x24) hasDollar = true
                    j++
                }
                if (j >= n) {
                    if (n - i <= MAX_CARRY) {
                        for (k in i until n) carry.add(bytes[k])
                    }
                    break
                }
                val finalByte = bytes[j].u()
                val drop = when (finalByte) {
                    0x63 -> true // DA
                    0x6E -> true // DSR
                    0x71 -> priv == 0x3E // XTVERSION, not DECSCUSR
                    0x70 -> hasDollar // DECRQM, not DECSTR
                    else -> false
                }
                if (!drop) {
                    for (k in i..j) out.add(bytes[k])
                }
                i = j + 1
            } else if (next == 0x50) {
                if (i + 3 >= n) {
                    for (k in i until n) carry.add(bytes[k])
                    break
                }
                val d0 = bytes[i + 2].u()
                val d1 = bytes[i + 3].u()
                val isQuery = (d0 == 0x2B || d0 == 0x24) && d1 == 0x71
                when (val scan = scanDCSTerminator(bytes, i + 2)) {
                    is DCS.Terminated -> {
                        if (!isQuery) {
                            for (k in i until scan.end) out.add(bytes[k])
                        }
                        i = scan.end
                    }
                    DCS.TrailingEsc -> {
                        if (isQuery) {
                            discardingDCSQuery = true
                            carry.add(0x1B.toByte())
                        } else {
                            for (k in i until n) out.add(bytes[k])
                        }
                        i = n
                    }
                    DCS.Exhausted -> {
                        if (isQuery) discardingDCSQuery = true
                        else for (k in i until n) out.add(bytes[k])
                        i = n
                    }
                }
            } else {
                out.add(bytes[i])
                i++
            }
        }
        return out.toByteArray()
    }

    private fun scanDCSTerminator(bytes: ByteArray, start: Int): DCS {
        var j = start
        val n = bytes.size
        while (j < n) {
            val b = bytes[j].u()
            if (b == 0x07) return DCS.Terminated(j + 1)
            if (b == 0x1B) {
                if (j + 1 >= n) return DCS.TrailingEsc
                if (bytes[j + 1].u() == 0x5C) return DCS.Terminated(j + 2)
            }
            j++
        }
        return DCS.Exhausted
    }

    private sealed class DCS {
        class Terminated(val end: Int) : DCS()
        data object TrailingEsc : DCS()
        data object Exhausted : DCS()
    }

    private companion object {
        const val MAX_CARRY = 96
    }
}

private fun Byte.u(): Int = toInt() and 0xFF
