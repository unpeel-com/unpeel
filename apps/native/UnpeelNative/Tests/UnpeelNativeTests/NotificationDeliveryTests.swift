import XCTest
@testable import UnpeelNative

@MainActor
final class NotificationDeliveryTests: XCTestCase {
    func testMacObservationSuppressesOnlyLocalEffects() {
        let policy = UnpeelStore.notificationDeliveryPolicy(
            macIsObserving: true,
            anyControllerIsViewing: false
        )

        XCTAssertFalse(policy.markUnread)
        XCTAssertFalse(policy.sendDesktop)
        // Phone delivery is intentionally not part of this local policy. The
        // caller still fans out through Link and filters each phone by id.
    }

    func testUnobservedTransitionPublishesLocalEffects() {
        let policy = UnpeelStore.notificationDeliveryPolicy(
            macIsObserving: false,
            anyControllerIsViewing: false
        )

        XCTAssertTrue(policy.markUnread)
        XCTAssertTrue(policy.sendDesktop)
    }

    func testControllerViewerSuppressesLocalEffects() {
        let policy = UnpeelStore.notificationDeliveryPolicy(
            macIsObserving: false,
            anyControllerIsViewing: true
        )

        XCTAssertFalse(policy.markUnread)
        XCTAssertFalse(policy.sendDesktop)
    }

    func testViewerSuppressionIsPerDevice() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("viewer-presence-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        // The worker publishes authenticated output leases beside presence.json.
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        try Data("""
        {"version":1,"updated_at":\(now),"sessions":{"session-1":[{"ip":"127.0.0.1","kind":"ws","device":"Alice's iPhone (phone-a)","device_id":"phone-a","last_seen":\(now)}]}}
        """.utf8).write(to: directory.appendingPathComponent("mobile-presence.json"))
        let store = ViewerPresenceStore(
            presenceURL: directory.appendingPathComponent("presence.json")
        )

        XCTAssertTrue(store.isDeviceViewing(sessionID: "session-1", deviceID: "phone-a"))
        XCTAssertFalse(store.isDeviceViewing(sessionID: "session-1", deviceID: "phone-b"))
        XCTAssertFalse(store.isDeviceViewing(sessionID: "session-2", deviceID: "phone-a"))
    }

    func testRemotePresenceExtractsStableDeviceID() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("remote-presence-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let presenceURL = directory.appendingPathComponent("presence.json")
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let body = """
        {"version":1,"updated_at":\(now),"sessions":{"session-1":[
          {"kind":"ws","device":"Alice's iPhone (phone-a)","last_seen":\(now)}
        ]}}
        """
        try Data(body.utf8).write(to: presenceURL)

        let store = ViewerPresenceStore(presenceURL: presenceURL)

        XCTAssertTrue(store.isDeviceViewing(sessionID: "session-1", deviceID: "phone-a"))
        XCTAssertFalse(store.isDeviceViewing(sessionID: "session-1", deviceID: "phone-b"))
    }

    func testCanonicalHostMobilePresenceFeedsTheSameViewerSurface() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("host-mobile-presence-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let now = Int64(Date().timeIntervalSince1970 * 1_000)
        let body = """
        {"version":1,"updated_at":\(now),"sessions":{"session-1":[
          {"kind":"mobile","device":"Alice's iPhone (phone-a)","last_seen":\(now)}
        ]}}
        """
        try Data(body.utf8).write(
            to: directory.appendingPathComponent("mobile-presence.json")
        )

        let store = ViewerPresenceStore(
            presenceURL: directory.appendingPathComponent("presence.json")
        )

        XCTAssertTrue(store.hasLiveMobileViewer(sessionID: "session-1"))
        XCTAssertTrue(store.isDeviceViewing(sessionID: "session-1", deviceID: "phone-a"))
        XCTAssertEqual(store.viewers["session-1"]?.first?.kind, .mobile)
    }

    func testMenuPromptNotifiesOncePerFalseToTrueEdge() {
        var decision = UnpeelStore.menuPromptNotificationDecision(
            previous: nil,
            runtimeGeneration: 1,
            active: false,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertFalse(decision.sendNotification)

        decision = UnpeelStore.menuPromptNotificationDecision(
            previous: decision.state,
            runtimeGeneration: 1,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertTrue(decision.sendNotification)
        XCTAssertTrue(decision.state.notificationSent)

        decision = UnpeelStore.menuPromptNotificationDecision(
            previous: decision.state,
            runtimeGeneration: 1,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertFalse(decision.sendNotification)

        decision = UnpeelStore.menuPromptNotificationDecision(
            previous: decision.state,
            runtimeGeneration: 1,
            active: false,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertFalse(decision.sendNotification)

        decision = UnpeelStore.menuPromptNotificationDecision(
            previous: decision.state,
            runtimeGeneration: 1,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertTrue(decision.sendNotification)
    }

    func testInitialActiveMenuSeedsWithoutNotification() {
        let decision = UnpeelStore.menuPromptNotificationDecision(
            previous: nil,
            runtimeGeneration: 1,
            active: true,
            initialAppScan: true,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )

        XCTAssertFalse(decision.sendNotification)
        XCTAssertFalse(decision.state.notificationSent)

        let hook = UnpeelStore.permissionRequestNotificationDecision(
            previous: decision.state,
            runtimeGeneration: 1
        )
        XCTAssertTrue(hook.sendNotification)
        XCTAssertTrue(hook.state?.notificationSent == true)
    }

    func testFirstActiveMenuAfterInitialScanNotifies() {
        let decision = UnpeelStore.menuPromptNotificationDecision(
            previous: nil,
            runtimeGeneration: 1,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )

        XCTAssertTrue(decision.sendNotification)
        XCTAssertTrue(decision.state.notificationSent)
    }

    func testMenuAndPermissionRequestDeduplicateInEitherOrder() {
        let armed = UnpeelStore.menuPromptNotificationDecision(
            previous: nil,
            runtimeGeneration: 4,
            active: false,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        ).state
        let menuFirst = UnpeelStore.menuPromptNotificationDecision(
            previous: armed,
            runtimeGeneration: 4,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertTrue(menuFirst.sendNotification)
        XCTAssertFalse(UnpeelStore.permissionRequestNotificationDecision(
            previous: menuFirst.state,
            runtimeGeneration: 4
        ).sendNotification)

        let hookFirstMenu = UnpeelStore.menuPromptNotificationDecision(
            previous: armed,
            runtimeGeneration: 4,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: true
        )
        XCTAssertFalse(hookFirstMenu.sendNotification)
        XCTAssertTrue(hookFirstMenu.state.notificationSent)
    }

    func testPermissionHookBeforeFirstMenuSampleSuppressesBothDuplicates() {
        let hook = UnpeelStore.permissionRequestNotificationDecision(
            previous: nil,
            runtimeGeneration: 7
        )
        XCTAssertTrue(hook.sendNotification)
        XCTAssertNil(hook.state)

        let menu = UnpeelStore.menuPromptNotificationDecision(
            previous: hook.state,
            runtimeGeneration: 7,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: true
        )
        XCTAssertFalse(menu.sendNotification)
        XCTAssertTrue(menu.state.notificationSent)

        XCTAssertFalse(UnpeelStore.permissionRequestNotificationDecision(
            previous: menu.state,
            runtimeGeneration: 7
        ).sendNotification)
    }

    func testRuntimeGenerationChangeRearmsAnActiveMenu() {
        let previous = UnpeelStore.MenuPromptNotificationState(
            runtimeGeneration: 2,
            active: true,
            notificationSent: true
        )
        let decision = UnpeelStore.menuPromptNotificationDecision(
            previous: previous,
            runtimeGeneration: 3,
            active: true,
            initialAppScan: false,
            detectionEnabled: true,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )

        XCTAssertTrue(decision.sendNotification)
        XCTAssertEqual(decision.state.runtimeGeneration, 3)
    }

    func testDisabledMenuDetectionDoesNotSuppressLaterPermissionHook() {
        let armed = UnpeelStore.menuPromptNotificationDecision(
            previous: nil,
            runtimeGeneration: 1,
            active: false,
            initialAppScan: false,
            detectionEnabled: false,
            dismissed: false,
            hookAlreadyNeedsInput: false
        ).state
        let menu = UnpeelStore.menuPromptNotificationDecision(
            previous: armed,
            runtimeGeneration: 1,
            active: true,
            initialAppScan: false,
            detectionEnabled: false,
            dismissed: false,
            hookAlreadyNeedsInput: false
        )
        XCTAssertFalse(menu.sendNotification)
        XCTAssertFalse(menu.state.notificationSent)
        XCTAssertTrue(UnpeelStore.permissionRequestNotificationDecision(
            previous: menu.state,
            runtimeGeneration: 1
        ).sendNotification)
    }
}
