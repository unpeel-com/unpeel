package com.unpeel.controller

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalOutputFramesTest {
    @Test
    fun freshSubscribeAsksForTheScreenSnapshot() {
        val url = TerminalOutputFrames.webSocketUrl(
            endpoint = "https://192.168.1.20:7443",
            port = 50123,
            sessionID = "sess-abc",
            token = "tok",
            offset = null,
        )
        assertEquals(
            "wss://192.168.1.20:50123/api/sessions/sess-abc/output?token=tok&snapshot=1",
            url,
        )
    }

    @Test
    fun resumeCarriesTheOffsetAndSkipsTheSnapshot() {
        val url = TerminalOutputFrames.webSocketUrl(
            endpoint = "https://192.168.1.20:7443",
            port = 50123,
            sessionID = "sess-abc",
            token = "tok+/=",
            offset = 42,
        )
        assertEquals(
            "wss://192.168.1.20:50123/api/sessions/sess-abc/output?token=tok%2B%2F%3D&offset=42",
            url,
        )
        assertFalse(url!!.contains("snapshot"))
    }

    @Test
    fun missingHostIsNotASocket() {
        assertNull(
            TerminalOutputFrames.webSocketUrl(
                endpoint = "not a url",
                port = 50123,
                sessionID = "sess-abc",
                token = "tok",
                offset = null,
            ),
        )
    }

    @Test
    fun helloKeepsTheModePreambleAndTheSnapshotFlag() {
        val text = """
            {"type":"hello","protocol":1,"session_id":"sess-abc","state":"running",
             "output_size":90,"requested_offset":null,"start_offset":80,"rebased":false,
             "cols":54,"rows":28,"mode_preamble_base64":"G1s/MTAwMGg=","snapshot":true}
        """.trimIndent()
        val hello = TerminalOutputFrames.parseText(text) as TerminalOutputText.Hello
        assertEquals("sess-abc", hello.sessionId)
        assertEquals(80L, hello.startOffset)
        assertEquals(54, hello.cols)
        assertEquals(28, hello.rows)
        assertFalse(hello.rebased)
        assertTrue(hello.snapshot)
        assertArrayEquals("\u001b[?1000h".toByteArray(), hello.modePreamble)
    }

    @Test
    fun baselineIsTheScreenAtAJournalOffset() {
        val text = """
            {"type":"baseline","journal_offset":80,"cols":54,"rows":28,"bytes_base64":"aGk="}
        """.trimIndent()
        val baseline = TerminalOutputFrames.parseText(text) as TerminalOutputText.Baseline
        assertEquals(80L, baseline.journalOffset)
        assertEquals(54, baseline.cols)
        assertArrayEquals("hi".toByteArray(), baseline.bytes)
    }

    @Test
    fun binaryFrameIsAnOffsetPlusRawBytes() {
        val frame = byteArrayOf(0, 0, 0, 0, 0, 0, 0, 42, 'a'.code.toByte(), 'b'.code.toByte())
        val parsed = TerminalOutputFrames.parseBinary(frame)!!
        assertEquals(42L, parsed.offset)
        assertArrayEquals("ab".toByteArray(), parsed.payload)
    }

    @Test
    fun shortBinaryFrameIsDropped() {
        assertNull(TerminalOutputFrames.parseBinary(byteArrayOf(1, 2, 3)))
    }
}
