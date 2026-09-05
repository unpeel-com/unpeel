import XCTest
@testable import UnpeelNative

final class HookServerParsingTests: XCTestCase {
    func testLoopbackSurfaceKeepsCallbacksAndAnswersNoHostRoute() {
        let clientRoutes = [
            "/_unpeel/platform-adapter/call",
            "/state-changed",
            "/show-window",
            "/reload-appearance",
        ]
        let hostRoutes = [
            "/mcp/sidebar",
            "/mcp/begin-pairing",
            "/hook/session-1",
            "/notify/session-1",
            "/app-theme/session-1",
            "/app-context/session-1",
            "/open-in-editor/session-1",
            "/mobile/bootstrap",
        ]

        for path in clientRoutes {
            XCTAssertTrue(HookServer.shouldDispatch(path), path)
        }
        for path in hostRoutes {
            XCTAssertFalse(HookServer.shouldDispatch(path), path)
        }
        XCTAssertEqual(LocalHostClientFeature.controllerOwnerHeaderValue, "serve")
    }

    func testPlatformAdapterBearerRequiresExactLongToken() {
        let token = "0123456789abcdef0123456789abcdef"
        XCTAssertTrue(HookServer.platformAdapterAuthorizationMatches(
            "Bearer \(token)", token: token
        ))
        XCTAssertTrue(HookServer.platformAdapterAuthorizationMatches(
            "bearer \(token)", token: token
        ))
        XCTAssertFalse(HookServer.platformAdapterAuthorizationMatches(
            "Bearer \(token)x", token: token
        ))
        XCTAssertFalse(HookServer.platformAdapterAuthorizationMatches(
            "Bearer \(token)" + String(repeating: "\0", count: 256), token: token
        ))
        XCTAssertFalse(HookServer.platformAdapterAuthorizationMatches(
            "Bearer \(token)", token: nil
        ))
        XCTAssertFalse(HookServer.platformAdapterAuthorizationMatches(
            "Bearer short", token: "short"
        ))
    }

    func testPlatformAdapterCallAcceptsOnlyTypedRegisteredOperation() {
        let valid = Data(
            #"{"version":1,"operation":"session.notify_when_done.set","request":{"sessionID":"session-1","notifyWhenDone":true}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: valid),
            .success(.setNotifyWhenDone(sessionID: "session-1", enabled: true))
        )

        let computerStatus = Data(
            #"{"version":1,"operation":"computer.status","request":{}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: computerStatus),
            .success(.computerStatus)
        )
        let invalidComputerStatus = Data(
            #"{"version":1,"operation":"computer.status","request":{"platform":"macOS"}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: invalidComputerStatus),
            .failure(.invalidEnvelope)
        )

        let overlay = Data(
            #"{"version":1,"operation":"overlay.snapshot","request":{}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: overlay),
            .success(.overlaySnapshot)
        )

        let thumbnail = Data(
            #"{"version":1,"operation":"artifact.thumbnail","request":{"query":{"session_id":"session-1","kind":"screenshots","name":"shot.png","offset":"0","limit":"128","max_dim":"320"}}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: thumbnail),
            .success(.thumbnail(query: [
                "session_id": "session-1",
                "kind": "screenshots",
                "name": "shot.png",
                "offset": "0",
                "limit": "128",
                "max_dim": "320",
            ]))
        )
        let invalidThumbnail = Data(
            #"{"version":1,"operation":"artifact.thumbnail","request":{"query":{"session_id":"session-1","kind":"screenshots","name":"shot.png","max_dim":"0"}}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: invalidThumbnail),
            .failure(.invalidEnvelope)
        )

        let link = Data(
            #"{"version":1,"operation":"link.entitlement.refresh","request":{"macID":" host-1 "}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: link),
            .success(.refreshLinkEntitlement(macID: "host-1"))
        )

        for (body, expected) in [
            (
                #"{"version":1,"operation":"mobile.e2e-key.reconcile","request":{"action":"sync"}}"#,
                PlatformAdapterCall.reconcileMobileE2EKeys
            ),
            (
                #"{"version":1,"operation":"mobile.e2e-key.reconcile","request":{"action":"remove","deviceID":" phone-1 "}}"#,
                PlatformAdapterCall.removeMobileE2EKey(deviceID: "phone-1")
            ),
        ] {
            XCTAssertEqual(
                HookServer.platformAdapterCall(from: Data(body.utf8)),
                .success(expected)
            )
        }

        let projectColor = Data(
            #"{"version":1,"operation":"overlay.project-color.set","request":{"projectID":" native-project ","colorID":"amber"}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: projectColor),
            .success(.setProjectFolderColor(projectID: "native-project", colorID: "amber"))
        )
        let clearProjectColor = Data(
            #"{"version":1,"operation":"overlay.project-color.set","request":{"projectID":"native-project","colorID":""}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: clearProjectColor),
            .success(.setProjectFolderColor(projectID: "native-project", colorID: nil))
        )
        let invalidProjectColor = Data(
            #"{"version":1,"operation":"overlay.project-color.set","request":{"projectID":"native-project","colorID":"plaid"}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: invalidProjectColor),
            .failure(.invalidEnvelope)
        )

        let numericBool = Data(
            #"{"version":1,"operation":"session.notify_when_done.set","request":{"sessionID":"session-1","notifyWhenDone":1}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: numericBool),
            .failure(.invalidEnvelope)
        )

        let push = Data(
            #"{"version":1,"operation":"push.register","request":{"deviceID":" phone-1 ","apnsToken":"0011223344556677","environment":"sandbox"}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: push),
            .success(.registerPushToken(
                deviceID: "phone-1",
                token: "0011223344556677",
                environment: "sandbox"
            ))
        )

        let invalidPush = Data(
            #"{"version":1,"operation":"push.register","request":{"deviceID":"phone-1","apnsToken":"not-hex-not-hex","environment":"sandbox"}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: invalidPush),
            .failure(.invalidEnvelope)
        )

        let relay = Data(
            #"{"version":1,"operation":"relay.credentials.recover","request":{"deviceID":"phone-1"}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: relay),
            .success(.recoverRelayCredentials(deviceID: "phone-1"))
        )

        let approvals = Data(
            #"{"version":1,"operation":"approval.present","request":{"approvals":[{"id":"approval-1","kind":"write","title":"Allow write?","body":"Session A → Session B","callerSessionID":"session-a","targetSessionID":"session-b","requestedAtUnixMs":1234}]}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: approvals),
            .success(.presentApprovals([
                PlatformPresentedApproval(
                    id: "approval-1",
                    kind: "write",
                    title: "Allow write?",
                    body: "Session A → Session B",
                    callerSessionID: "session-a",
                    targetSessionID: "session-b",
                    requestedAtUnixMs: 1234
                ),
            ]))
        )

        let notification = Data(
            #"{"version":1,"operation":"notification.deliver","request":{"sessionID":"session-1","title":"Research","body":"Finished","kind":"done","requiresNotifyWhenDone":true,"sendDesktop":true,"suppressDeviceIDs":["phone-a"]}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: notification),
            .success(.deliverNotification(
                sessionID: "session-1",
                title: "Research",
                body: "Finished",
                kind: "done",
                requiresNotifyWhenDone: true,
                sendDesktop: true,
                suppressDeviceIDs: ["phone-a"]
            ))
        )

        let invalidNotification = Data(
            #"{"version":1,"operation":"notification.deliver","request":{"sessionID":"session-1","title":"Research","body":"Finished","kind":"done","requiresNotifyWhenDone":1,"sendDesktop":true,"suppressDeviceIDs":[]}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: invalidNotification),
            .failure(.invalidEnvelope)
        )

        let unknown = Data(
            #"{"version":1,"operation":"platform.unknown","request":{}}"#.utf8
        )
        XCTAssertEqual(
            HookServer.platformAdapterCall(from: unknown),
            .failure(.unsupportedOperation)
        )
    }

    func testPortRegistryRegistrationPrunesOnlyProvenStaleEntries() {
        let reconciled = HookServer.reconciledPortRegistry(
            [41_000, 41_001, 41_000, 0, 41_002, 41_003],
            registering: 41_003,
            isDefinitelyStale: { $0 == 41_001 }
        )

        XCTAssertEqual(reconciled, [41_000, 41_002, 41_003])
    }

    func testPortRegistryRegistrationKeepsNewestSixteenEntries() {
        let existing = (41_000..<41_020).map(UInt16.init)
        let reconciled = HookServer.reconciledPortRegistry(
            existing,
            registering: 42_000,
            isDefinitelyStale: { _ in false }
        )

        XCTAssertEqual(reconciled, Array(existing.suffix(15)) + [42_000])
    }

    func testContentLengthParserRejectsNegativeAndAmbiguousValues() throws {
        XCTAssertEqual(try StrictHTTPContentLength.parse(nil), 0)
        XCTAssertEqual(try StrictHTTPContentLength.parse("0"), 0)
        XCTAssertEqual(try StrictHTTPContentLength.parse("42"), 42)

        for invalid in ["-1", "+1", "", "1, 1", " 1", "1 ", "nope"] {
            XCTAssertThrowsError(try StrictHTTPContentLength.parse(invalid)) { error in
                XCTAssertEqual(error as? StrictHTTPContentLengthError, .invalid)
            }
        }
        XCTAssertThrowsError(
            try StrictHTTPContentLength.parse(String(StrictHTTPContentLength.maximum + 1))
        ) { error in
            XCTAssertEqual(error as? StrictHTTPContentLengthError, .tooLarge)
        }
    }
}
