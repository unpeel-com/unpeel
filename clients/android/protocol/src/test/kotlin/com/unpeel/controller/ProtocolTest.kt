package com.unpeel.controller

import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
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
        assertNull(payload.publicEndpoint)
        assertEquals(1893456000_000L, payload.expiresAtUnixMs)
    }

    @Test
    fun compactPairingCodeKeepsThePublicAddressBesideTheLanAddress() {
        val payload = PairingCode.decode(
            "UNPEEL:1:10.0.0.8:53621:AbC-1:TokenValue:1893456000:203.0.113.8:8443",
        )
        assertEquals("http://10.0.0.8:53621/mobile", payload?.endpoint)
        assertEquals("http://203.0.113.8:8443/mobile", payload?.publicEndpoint)
        assertNull(PairingCode.decode("UNPEEL:1:10.0.0.8:53621:AbC-1:TokenValue:1893456000:203.0.113.8:nope"))
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
        assertNull(snapshot.workspaces.single().tintHue)
        assertNull(snapshot.hostTintHue)
        assertFalse(snapshot.experimentalWorktreesEnabled)
        assertNull(snapshot.sessions.single().capabilities)
        assertFalse(snapshot.sessions.single().notifyWhenDone)
    }

    @Test
    fun bootstrapReadsSidebarAndSessionCapabilities() {
        val snapshot = BootstrapJson.parse(
            """
            {"hostProtocol":{"capabilities":["session.runtime.resume","project.organization.set"]},
             "hostTintHue":0,"experimentalWorktreesEnabled":true,
             "folders":[{"id":"f","name":"Work","colorID":"blue","parentFolderID":"root","sortOrder":0},{"id":"empty","name":"Empty","sortOrder":1}],
             "projects":[
               {"id":"p","name":"upreel","path":"/code","folderID":"f","sortOrder":0,"colorID":"sky","gitBranch":"main","dateSorted":false,"sessionOrder":["s1","wt"]},
               {"id":"loose","name":"notes","path":"/n","sortOrder":1},
               {"id":"wt","name":"feature","path":"/wt","parentProjectID":"p","worktreeBranch":"feature","sortOrder":2},
               {"id":"sidebar-p","name":"Sidebar","path":"/code","parentProjectID":"p","isGroup":true,"sortOrder":3},
               {"id":"grp","name":"Ideas","path":"/code","parentProjectID":"p","isGroup":true,"pinned":true,"sortOrder":4}
             ],
             "sessions":[
               {"id":"s1","projectID":"p","title":"a","command":"cursor","status":"running","activity":"idle","providerID":"cursor","notifyWhenDone":true,"capabilities":{"restart":true,"resumeAgent":true,"notifyWhenDone":true,"resumable":true}},
               {"id":"s2","projectID":"sidebar-p","title":"hoisted","command":"/bin/bash","status":"running","activity":"idle"},
               {"id":"s3","projectID":"wt","title":"old","command":"amp","status":"exited","activity":"idle","runtimeLaunchPending":true,"capabilities":{"restart":true,"notifyWhenDone":false,"archive":true}},
               {"id":"s4","projectID":"loose","title":"shell","command":"claude","status":"running","activity":"idle","activeRuntimeID":"claude","capabilities":{"restart":false,"resumeAgent":true,"notifyWhenDone":false,"resumable":false}}
             ],
             "presets":[]}
            """.trimIndent(),
        )
        assertEquals(0.0, snapshot.hostTintHue)
        assertTrue(snapshot.experimentalWorktreesEnabled)
        assertEquals("blue", snapshot.folders.first().colorID)
        assertEquals("root", snapshot.folders.first().parentFolderID)
        assertEquals("sky", snapshot.projects.first().colorID)
        val index = sidebarIndex(snapshot)
        assertEquals(listOf("p"), index.folderGroups.single().projects.map { it.id })
        assertEquals(listOf("loose"), index.looseProjects.map { it.id })
        assertEquals(listOf("f", "empty"), snapshot.folders.map { it.id })
        assertEquals(setOf("s2"), index.hoistedSessionIDs)
        assertEquals(setOf("wt", "grp"), index.childrenByParent["p"].orEmpty().map { it.id }.toSet())
        assertEquals(setOf("s1", "s2"), index.sessionsByProject["p"].orEmpty().map { it.id }.toSet())
        val rows = sidebarEntries(
            snapshot.projects.first { it.id == "p" },
            index.sessionsByProject["p"].orEmpty(),
            index.childrenByParent["p"].orEmpty(),
            index.hoistedSessionIDs,
            worktreesEnabled = true,
        )
        assertEquals(listOf("s2", "grp", "s1", "wt"), rows.map { entry ->
            when (entry) {
                is SidebarEntry.Session -> entry.session.id
                is SidebarEntry.Project -> entry.project.id
            }
        })
        val hidden = sidebarEntries(
            snapshot.projects.first { it.id == "p" },
            index.sessionsByProject["p"].orEmpty(),
            index.childrenByParent["p"].orEmpty(),
            index.hoistedSessionIDs,
            worktreesEnabled = false,
        )
        assertFalse(hidden.any { it is SidebarEntry.Project && it.project.id == "wt" })
        val running = snapshot.sessions.first { it.id == "s1" }
        assertEquals(ResumeOffer.Agent, resumeOffer(running, snapshot.capabilities))
        assertEquals(ResumeOffer.None, resumeOffer(running.copy(runtimeLaunchPending = true), snapshot.capabilities))
        assertEquals(ResumeOffer.None, resumeOffer(running, emptyList()))
        val exited = snapshot.sessions.first { it.id == "s3" }
        assertTrue(exited.capabilities!!.resumable)
        assertEquals(ResumeOffer.Session, resumeOffer(exited, snapshot.capabilities))
        assertEquals(ResumeOffer.None, resumeOffer(exited.copy(capabilities = null), snapshot.capabilities))
        assertTrue(canNotifyWhenDone(snapshot.sessions.first { it.id == "s2" }))
        assertFalse(canNotifyWhenDone(exited))
        assertTrue(AgentCatalog.supportsTranscript("cursor", "cursor"))
        assertFalse(AgentCatalog.supportsTranscript(null, "amp"))
        assertFalse(AgentCatalog.supportsTranscript(null, "/bin/bash"))
        assertTrue(AgentCatalog.isAgent(null, "amp"))
        assertFalse(AgentCatalog.isAgent(null, "bash"))
        val observed = snapshot.sessions.first { it.id == "s4" }
        assertEquals(ResumeOffer.None, resumeOffer(observed, snapshot.capabilities))
        assertTrue(AgentCatalog.supportsTranscript(null, "claude"))
    }

    @Test
    fun sidebarKeepsHostArrayOrderAheadOfSortOrder() {
        val snapshot = BootstrapJson.parse(
            """
            {"folders":[
               {"id":"later","name":"Later","sortOrder":0},
               {"id":"sooner","name":"Sooner","sortOrder":9}
             ],
             "projects":[
               {"id":"z","name":"Z","sortOrder":0},
               {"id":"a","name":"A","folderID":"sooner","sortOrder":5},
               {"id":"m","name":"M","folderID":"sooner","sortOrder":1},
               {"id":"child-b","name":"B","parentProjectID":"z","worktreeBranch":"b","sortOrder":0},
               {"id":"child-a","name":"A","parentProjectID":"z","worktreeBranch":"a","sortOrder":3}
             ],
             "sessions":[]}
            """.trimIndent(),
        )
        assertEquals(listOf("later", "sooner"), snapshot.folders.map { it.id })
        assertEquals(listOf("z", "a", "m", "child-b", "child-a"), snapshot.projects.map { it.id })
        val index = sidebarIndex(snapshot)
        assertEquals(listOf("z"), index.looseProjects.map { it.id })
        assertEquals(listOf("sooner"), index.folderGroups.map { it.folder.id })
        assertEquals(listOf("a", "m"), index.folderGroups.single().projects.map { it.id })
        assertEquals(listOf("child-b", "child-a"), index.childrenByParent["z"].orEmpty().map { it.id })
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
        assertEquals("hi\nthere[31m", TerminalInput.sanitize("hi\r\nthere\u001b[31m"))
        assertEquals(true, TerminalInput.containsPath("look at src/lib/foo.ts"))
        assertEquals(false, TerminalInput.containsPath("capture the result"))
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
