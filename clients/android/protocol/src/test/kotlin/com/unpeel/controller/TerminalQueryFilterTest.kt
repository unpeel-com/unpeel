package com.unpeel.controller

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalQueryFilterTest {
    private fun strip(text: String): String = String(TerminalQueryFilter().strip(text.toByteArray()), Charsets.UTF_8)

    private fun strip(chunks: List<String>): String {
        val filter = TerminalQueryFilter()
        val out = java.io.ByteArrayOutputStream()
        for (chunk in chunks) out.write(filter.strip(chunk.toByteArray()))
        return out.toString(Charsets.UTF_8)
    }

    @Test
    fun stripsXTVersionRequest() {
        assertEquals("AB", strip("A\u001B[>qB"))
        assertEquals("AB", strip("A\u001B[>0qB"))
    }

    @Test
    fun stripsDeviceAttributesAndStatusRequests() {
        assertEquals("xy", strip("x\u001B[cy"))
        assertEquals("xy", strip("x\u001B[0cy"))
        assertEquals("xy", strip("x\u001B[>cy"))
        assertEquals("xy", strip("x\u001B[6ny"))
        assertEquals("xy", strip("x\u001B[?6ny"))
    }

    @Test
    fun stripsDECRQMButNotDECSTR() {
        assertEquals("ab", strip("a\u001B[?2026\$pb"))
        assertEquals("a\u001B[!pb", strip("a\u001B[!pb"))
    }

    @Test
    fun stripsDCSQueries() {
        assertEquals("ab", strip("a\u001BP+q544e\u001B\\b"))
        assertEquals("ab", strip("a\u001BP\$qm\u001B\\b"))
    }

    @Test
    fun preservesRISAndCursorStyleAndOrdinarySequences() {
        assertEquals("a\u001Bcb", strip("a\u001Bcb"))
        assertEquals("a\u001B[2 qb", strip("a\u001B[2 qb"))
        assertEquals("\u001B[31mhi\u001B[0m\u001B[2J", strip("\u001B[31mhi\u001B[0m\u001B[2J"))
    }

    @Test
    fun stripsQuerySplitAcrossChunks() {
        assertEquals("okdone", strip(listOf("ok\u001B[>0", "qdone")))
        assertEquals("ab", strip(listOf("a\u001B[?20", "26\$pb")))
        assertEquals("xy", strip(listOf("x\u001B", "[6ny")))
    }

    @Test
    fun emitsNonQuerySplitSequenceIntact() {
        assertEquals("\u001B[31mred", strip(listOf("\u001B[3", "1mred")))
        assertEquals("a\u001Bcb", strip(listOf("a\u001B", "cb")))
    }

    @Test
    fun stripsDCSQuerySplitAcrossChunks() {
        assertEquals("ab", strip(listOf("a\u001BP+q54", "4e\u001B", "\\b")))
        assertEquals("ab", strip(listOf("a\u001BP", "\$qm\u001B\\b")))
        val longPayload = "5".repeat(4096)
        assertEquals("ab", strip(listOf("a\u001BP+q", longPayload, "\u001B\\b")))
    }

    @Test
    fun oversizedIncompleteCSIIsDropped() {
        val junk = "\u001B[" + "1;".repeat(128)
        assertEquals("oklater", strip(listOf("ok", junk, "later")))
    }

    @Test
    fun resetClearsCarriedState() {
        val filter = TerminalQueryFilter()
        filter.strip("a\u001B[>0".toByteArray())
        filter.reset()
        assertEquals("fresh", String(filter.strip("fresh".toByteArray()), Charsets.UTF_8))
    }

    @Test
    fun plainTextIsUnchanged() {
        assertEquals("plain text 123", strip("plain text 123"))
    }

    @Test
    fun outputChunkReadsTruncation() {
        val preamble = "\u001B[?1049h".toByteArray()
        val chunk = BootstrapJson.outputChunk(
            """{"offset":10,"nextOffset":12,"dataBase64":"${b64("ab".toByteArray())}","truncated":true,"modePreambleBase64":"${b64(preamble)}"}""",
        )
        assertTrue(chunk.truncated)
        assertEquals(10L, chunk.offset)
        assertEquals(12L, chunk.nextOffset)
        assertArrayEquals("ab".toByteArray(), chunk.data)
        assertArrayEquals(preamble, chunk.modePreamble)
        val older = BootstrapJson.outputChunk("""{"offset":0,"nextOffset":0,"dataBase64":""}""")
        assertFalse(older.truncated)
    }
}
