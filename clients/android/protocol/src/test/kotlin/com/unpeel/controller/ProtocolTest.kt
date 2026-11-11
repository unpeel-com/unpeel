package com.unpeel.controller

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class ProtocolTest {
    @Test
    fun compactPairingCodeRoundTripsTheEndpoint() {
        val payload = PairingCode.decode("UNPEEL:1:10.0.0.8:53621:AbC-1:TokenValue:1893456000")
        assertNotNull(payload)
        assertEquals(1, payload!!.protocolVersion)
        assertEquals("abc-1", payload.macID)
        assertEquals("TokenValue", payload.token)
        assertEquals("http://10.0.0.8:53621/mobile", payload.endpoint)
        assertEquals(1893456000_000L, payload.expiresAtUnixMs)
    }

    @Test
    fun jsonPairingCodeKeepsTheFingerprint() {
        val payload = PairingCode.decode(
            """{"protocolVersion":1,"macID":"host","endpoint":"http://127.0.0.1:9/mobile","token":"t","expiresAtUnixMs":5,"certificateFingerprint":"ab"}""",
        )
        assertEquals("ab", payload?.certificateFingerprint)
        assertNull(PairingCode.decode("not a code"))
    }

    @Test
    fun pairingResponseMustNameThisDeviceAndAMobileRoute() {
        val payload = PairingCode.decode("UNPEEL:1:10.0.0.8:53621:AbC-1:TokenValue:1893456000")!!
        val key = b64(ByteArray(32) { it.toByte() })
        val accepted = acceptPairingResponse(
            pairingResponse(key, "wss://link.example/relay", "https://10.0.0.8:53621/mobile"),
            payload,
            "phone-1",
        )
        assertEquals("abc-1", accepted.macID)
        assertEquals("https://10.0.0.8:53621/mobile", accepted.directEndpoint)
        assertThrows(PairingRejected::class.java) {
            acceptPairingResponse(
                pairingResponse(key, "https://link.example/relay", "https://10.0.0.8:53621/mobile"),
                payload,
                "phone-1",
            )
        }
        assertThrows(PairingRejected::class.java) {
            acceptPairingResponse(
                pairingResponse(key, "wss://link.example/relay", "https://10.0.0.8:53621/other"),
                payload,
                "phone-1",
            )
        }
    }

    private fun pairingResponse(key: String, relayURL: String, direct: String) = JSONObject(
        """{"protocolVersion":1,"macID":"abc-1","macName":"Studio","endpoint":"http://10.0.0.8:53621/mobile","directEndpoint":"$direct","deviceID":"phone-1","authToken":"auth","pairedAtUnixMs":1,"relayCredentials":{"relayURL":"$relayURL","macID":"abc-1","relayToken":"rt","e2eKeyB64":"$key"}}""",
    )

    @Test
    fun pairingBoxOpensWithTheSameEndpointString() {
        val plaintext = """{"token":"t","device":{"id":"phone","name":"Pixel","platform":"android"}}""".toByteArray()
        val envelope = PairingCrypto.seal(plaintext, "secret-token", "host-id", "http://10.0.0.2:1/mobile", PairingCrypto.REQUEST)
        val opened = PairingCrypto.open(envelope, "secret-token", "host-id", "http://10.0.0.2:1/mobile", PairingCrypto.REQUEST)
        assertArrayEquals(plaintext, opened)
    }

    @Test
    fun relayKnownAnswerMatchesSwiftAndRust() {
        val e2eKey = ByteArray(32) { it.toByte() }
        val sharedSecret = ByteArray(32) { (0x40 + it).toByte() }
        val clientSalt = ByteArray(16) { (0x10 + it).toByte() }
        val hostSalt = ByteArray(16) { (0xA0 + it).toByte() }
        val clientEph = ByteArray(32) { (0x80 + it).toByte() }
        val hostEph = ByteArray(32) { (0xC0 + it * 3).toByte() }
        val mac = RelayHandshake.transcriptMac(e2eKey, "phone-kat-1", clientSalt, hostSalt, clientEph, hostEph)
        assertEquals("+BBTo0DBUwkP829M9w6eviupf+3pv5XxzrtNnUeYNQc=", b64(mac))

        val client = RelaySession(e2eKey, sharedSecret, clientSalt, hostSalt, isHost = false)
        val sealed = client.seal("known-answer-plaintext".toByteArray())
        assertEquals("AAAAAAAAAAFXoFTergM+a27Rbw/LTzDUy/OhPJRbGDcDIEpfVPJbKdy1zzcoCQ==", b64(sealed))
        val host = RelaySession(e2eKey, sharedSecret, clientSalt, hostSalt, isHost = true)
        assertEquals("known-answer-plaintext", host.open(sealed).toString(Charsets.UTF_8))
    }

    @Test
    fun bootstrapIgnoresUnknownFields() {
        val snapshot = BootstrapJson.parse(
            """{"protocolVersion":1,"macName":"Studio","future":true,"folders":[],"projects":[{"id":"p","name":"upreel","path":"/code","sortOrder":1}],"presets":[],"sessions":[{"id":"s","projectID":"p","title":"fix","command":"grok","createdAtUnixMs":1,"status":"running","activity":"working","pinned":true}],"hostWorkspaces":[{"id":"w","name":"default","isCurrent":true,"isRunning":true}]}""",
        )
        assertEquals("Studio", snapshot.macName)
        assertEquals("working", snapshot.sessions.single().activity)
        assertEquals(true, snapshot.sessions.single().pinned)
        assertEquals(1L, snapshot.sessions.single().updatedAtUnixMs)
        assertEquals("default", snapshot.workspaces.single().name)
    }

    @Test
    fun terminalAddressesCellsAndKeepsTheHostGrid() {
        val screen = VtScreen(10, 4)
        screen.feed("\u001b[2J\u001b[1;1H\u001b[32mhi".toByteArray())
        val cells = screen.snapshot()
        assertEquals('h', cells[0][0].ch)
        assertEquals('i', cells[0][1].ch)
        assertEquals(0x0DBC79, cells[0][0].fg)
        screen.feed("\r\nnext".toByteArray())
        assertEquals('n', screen.snapshot()[1][0].ch)
    }

    @Test
    fun pasteAndSubmitUsesBracketedPaste() {
        assertEquals("\u001b[200~hello\u001b[201~", TerminalInput.paste("hello"))
        assertEquals("\u001b[200~hello\u001b[201~\r", TerminalInput.pasteAndSubmit("hello"))
        assertEquals("\u0003", TerminalInput.key("controlC"))
    }

    @Test
    fun scrollbackKeepsTheLineThatLeftTheTop() {
        val screen = VtScreen(4, 2)
        screen.feed("ab\r\ncd\r\nef".toByteArray())
        assertEquals('a', screen.scrollback().single()[0].ch)
        assertEquals('c', screen.snapshot()[0][0].ch)
        assertEquals('e', screen.snapshot()[1][0].ch)
    }

    @Test
    fun wideCharacterOccupiesTwoCells() {
        val screen = VtScreen(6, 2)
        screen.feed("中a".toByteArray())
        val row = screen.snapshot()[0]
        assertEquals('中', row[0].ch)
        assertEquals('\u0000', row[1].ch)
        assertEquals('a', row[2].ch)
        assertEquals(3, screen.cursor().second)
    }

    @Test
    fun tunnelRequestOmitsEmptyOptionals() {
        val json = JSONObject()
            .put("id", 1)
            .put("method", "GET")
            .put("path", "/mobile/bootstrap")
            .put("query", JSONObject())
        assertEquals("/mobile/bootstrap", json.getString("path"))
    }
}
