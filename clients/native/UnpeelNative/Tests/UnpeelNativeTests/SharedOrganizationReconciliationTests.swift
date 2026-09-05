import XCTest
@testable import UnpeelNative

@MainActor
final class SharedOrganizationReconciliationTests: XCTestCase {
    private func pin(
        _ id: String,
        projectID: String = "project",
        at pinnedAt: UInt64
    ) -> PinnedSidebarSession {
        PinnedSidebarSession(
            key: PinnedSidebarSession.key(forSessionID: id),
            projectID: projectID,
            sessionID: id,
            pinnedAt: pinnedAt
        )
    }

    private func rawPin(
        _ id: String,
        projectID: String = "project",
        at pinnedAt: UInt64,
        extra: [String: Any] = [:]
    ) -> [String: Any] {
        var row = extra
        row["key"] = PinnedSidebarSession.key(forSessionID: id)
        row["project_id"] = projectID
        row["session_id"] = id
        row["pinned_at"] = pinnedAt
        return row
    }

    private func groupedRows(_ object: [String: Any]) -> [String: [[String: Any]]] {
        guard let groups = object["pinned_sessions"] as? [String: Any] else {
            return [:]
        }
        return groups.reduce(into: [:]) { result, item in
            result[item.key] = (item.value as? [Any])?.compactMap {
                $0 as? [String: Any]
            } ?? []
        }
    }

    func testTitleResolutionUsesDurableFreshness() {
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: .init(title: "  Headless rename  ", updatedAt: 300),
                nativeTitle: "Stale native rename",
                pendingWriteAt: 200
            ),
            .init(title: "Headless rename", shouldPublishNative: false)
        )
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: .init(title: "Older shared rename", updatedAt: 200),
                nativeTitle: "Native write to retry",
                pendingWriteAt: 300
            ),
            .init(title: "Native write to retry", shouldPublishNative: true)
        )
    }

    func testLegacyPendingTitleNeverOverwritesAValidSharedMarker() {
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: .init(title: "Shared rename", updatedAt: 400),
                nativeTitle: "Legacy pending rename",
                pendingWriteAt: 0
            ),
            .init(title: "Shared rename", shouldPublishNative: false)
        )
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: .init(title: "Shared rename", updatedAt: nil),
                nativeTitle: "Timestamped native rename",
                pendingWriteAt: 500
            ),
            .init(title: "Shared rename", shouldPublishNative: false)
        )
    }

    func testPendingOrLegacyNativeTitlePublishesWhenNoValidMarkerExists() {
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: nil,
                nativeTitle: "  Native rename  ",
                pendingWriteAt: 300
            ),
            .init(title: "Native rename", shouldPublishNative: true)
        )
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: .init(title: "  ", updatedAt: 500),
                nativeTitle: "Legacy native rename",
                pendingWriteAt: nil
            ),
            .init(title: "Legacy native rename", shouldPublishNative: true)
        )
        XCTAssertEqual(
            UnpeelStore.resolvedSessionTitle(
                sharedMarker: nil,
                nativeTitle: "\n",
                pendingWriteAt: 300
            ),
            .init(title: nil, shouldPublishNative: false)
        )
    }

    func testLegacyPendingTitleStorageDecodesConservatively() throws {
        XCTAssertEqual(
            UnpeelStore.decodedPendingTitleWrites(["one", "two"]),
            ["one": 0, "two": 0]
        )
        let timestamped = try JSONEncoder().encode(["one": UInt64(123)])
        XCTAssertEqual(
            UnpeelStore.decodedPendingTitleWrites(timestamped),
            ["one": 123]
        )
    }

    func testPinIntentPreservesConcurrentUnrelatedSharedPin() {
        var object: [String: Any] = [
            "pinned_sessions": [
                "shared-project": [
                    rawPin(
                        "shared",
                        projectID: "shared-project",
                        at: 100,
                        extra: ["future_field": "keep"]
                    ),
                ],
            ],
        ]
        let native = pin("native", projectID: "native-project", at: 200)

        XCTAssertTrue(UnpeelStore.applyPinOverrides(.init(added: [native]), to: &object))

        let rows = groupedRows(object)
        XCTAssertEqual(
            Set(rows.values.joined().compactMap { $0["key"] as? String }),
            Set([PinnedSidebarSession.key(forSessionID: "shared"), native.key])
        )
        XCTAssertEqual(
            rows["shared-project"]?.first?["future_field"] as? String,
            "keep"
        )
    }

    func testPinRemovalTouchesOnlyItsOwnKey() {
        let removedKey = PinnedSidebarSession.key(forSessionID: "removed")
        var object: [String: Any] = [
            "pinned_sessions": [
                "project": [
                    rawPin("removed", at: 100),
                    rawPin("untouched", at: 200),
                ],
            ],
        ]

        XCTAssertTrue(UnpeelStore.applyPinOverrides(
            .init(removedKeys: [removedKey], removedAt: [removedKey: 300]),
            to: &object
        ))

        XCTAssertEqual(
            groupedRows(object).values.joined().compactMap { $0["key"] as? String },
            [PinnedSidebarSession.key(forSessionID: "untouched")]
        )
    }

    func testPinIntentKeepsLegacyFlatPinsAndNormalizesShape() {
        var object: [String: Any] = [
            "pinned_sessions": [rawPin("legacy", projectID: "old", at: 100)],
        ]
        let native = pin("native", projectID: "new", at: 200)

        XCTAssertTrue(UnpeelStore.applyPinOverrides(.init(added: [native]), to: &object))

        let rows = groupedRows(object)
        XCTAssertEqual(rows["old"]?.first?["session_id"] as? String, "legacy")
        XCTAssertEqual(rows["new"]?.first?["session_id"] as? String, "native")
    }

    func testMalformedPinStateLeavesIntentPending() {
        var object: [String: Any] = ["pinned_sessions": "not-a-pin-map"]
        let before = object["pinned_sessions"] as? String

        XCTAssertFalse(UnpeelStore.applyPinOverrides(
            .init(added: [pin("native", at: 200)]),
            to: &object
        ))
        XCTAssertEqual(object["pinned_sessions"] as? String, before)
    }

    func testNewerSharedUnpinRetiresStaleNativeAddedOverlay() {
        let nativePin = pin("session", at: 100)
        let reconciled = UnpeelStore.reconciledPinOverrides(
            .init(added: [nativePin]),
            sharedPins: [:],
            sharedStateModifiedAt: 200
        )

        XCTAssertTrue(reconciled.added.isEmpty)
    }

    func testPendingNativeAddSurvivesAnOlderSharedSnapshot() {
        let nativePin = pin("session", at: 300)
        let reconciled = UnpeelStore.reconciledPinOverrides(
            .init(added: [nativePin]),
            sharedPins: [:],
            sharedStateModifiedAt: 200
        )

        XCTAssertEqual(reconciled.added, [nativePin])
    }

    func testNewerSharedRepinRetiresNativeRemoval() {
        // Rust intentionally retains a pin's original ordering timestamp on
        // idempotent writes/moves. The containing app-state version therefore
        // proves the repin is newer even when `pinned_at` itself is older.
        let sharedPin = pin("session", projectID: "group", at: 100)
        let key = sharedPin.key
        let reconciled = UnpeelStore.reconciledPinOverrides(
            .init(removedKeys: [key], removedAt: [key: 200]),
            sharedPins: [key: sharedPin],
            sharedStateModifiedAt: 300
        )

        XCTAssertTrue(reconciled.removedKeys.isEmpty)
        XCTAssertTrue(reconciled.removedAt.isEmpty)
    }

    func testPendingNativeRemovalBeatsAnOlderSharedPin() {
        let sharedPin = pin("session", at: 200)
        let key = sharedPin.key
        let reconciled = UnpeelStore.reconciledPinOverrides(
            .init(removedKeys: [key], removedAt: [key: 300]),
            sharedPins: [key: sharedPin],
            sharedStateModifiedAt: 250
        )

        XCTAssertEqual(reconciled.removedKeys, [key])
        XCTAssertEqual(reconciled.removedAt[key], 300)
    }

    func testLegacyUntimestampedRemovalDefersToReadableSharedState() throws {
        let key = PinnedSidebarSession.key(forSessionID: "session")
        let legacy = try JSONDecoder().decode(
            UnpeelStore.NativePinOverrides.self,
            from: Data(#"{"added":[],"removedKeys":["session:session"]}"#.utf8)
        )
        XCTAssertTrue(legacy.removedAt.isEmpty)

        let reconciled = UnpeelStore.reconciledPinOverrides(
            legacy,
            sharedPins: [key: pin("session", at: 400)],
            sharedStateModifiedAt: 400
        )
        XCTAssertTrue(reconciled.removedKeys.isEmpty)
    }
}
