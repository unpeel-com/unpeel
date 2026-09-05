import XCTest
@testable import UnpeelNative

final class LocalHostClientFeatureTests: XCTestCase {
    func testLaunchProbeReportsUnavailableWhenTheServiceNeverAnswers() {
        var clock = Date(timeIntervalSince1970: 1_000)
        var probes = 0
        let resolution = LocalHostClientFeature.resolve(
            probe: { probes += 1; return false },
            deadline: 5,
            now: { clock },
            sleep: { clock = clock.addingTimeInterval($0) }
        )
        guard case .unavailable(let reason) = resolution else {
            return XCTFail("expected the unavailable outcome, got \(resolution)")
        }
        XCTAssertTrue(reason.contains("did not answer"), reason)
        XCTAssertGreaterThan(probes, 10)
        XCTAssertLessThan(probes, 60)
    }

    func testLaunchProbeSucceedsAsSoonAsTheServiceAnswers() {
        var answers = [false, false, true]
        let resolution = LocalHostClientFeature.resolve(
            probe: { answers.removeFirst() },
            deadline: 5,
            sleep: { _ in }
        )
        XCTAssertEqual(resolution, .client)
        XCTAssertTrue(answers.isEmpty)
    }

    /// The real probe against a home with no worker: the shim is simply an
    /// empty home whose host.sock never appears, the way a launch looks when
    /// the bundled service failed to start. The outcome is a status, never
    /// a second Host: the app stays a client.
    func testRealProbeAgainstAHomeWithoutAWorkerReportsUnavailableWithinTheDeadline() throws {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-no-worker-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let started = Date()
        let resolution = LocalHostClientFeature.resolve(
            probe: { LocalHostControl.probeBlocking(home: home.path) },
            deadline: 1
        )
        guard case .unavailable = resolution else {
            return XCTFail("expected unavailable, got \(resolution)")
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 4)
    }

    @MainActor
    func testServiceStateFailsClosedAndRecoversOnConnection() {
        let manager = HostServiceManager.shared
        manager.noteLaunchProbeStarted()
        XCTAssertEqual(manager.serviceState, .starting)
        // A slow first boot is not an error until the launch probe settles.
        manager.noteLocalConnectionFailed(reason: "socket missing")
        XCTAssertEqual(manager.serviceState, .starting)
        manager.noteLaunchProbeFailed(reason: "did not answer")
        XCTAssertEqual(manager.serviceState, .unavailable(reason: "did not answer"))
        XCTAssertEqual(manager.launchProbeFailureReason, "did not answer")
        // The Local client connecting proves the service live regardless.
        manager.noteLocalConnectionEstablished()
        XCTAssertEqual(manager.serviceState, .live)
        XCTAssertNil(manager.launchProbeFailureReason)
        // A later drop after the probe settled is reported, never hidden.
        manager.noteLocalConnectionFailed(reason: "worker exited")
        XCTAssertEqual(manager.serviceState, .unavailable(reason: "worker exited"))
        manager.noteLaunchProbeSucceeded()
        XCTAssertEqual(manager.serviceState, .live)
    }

    func testTheAppIsAlwaysTheControllerTransportOwner() {
        XCTAssertEqual(LocalHostClientFeature.controllerOwnerHeaderValue, "serve")
    }

    func testLocalDisplayFallsBackUntilACompleteHostProjectionIsReady() {
        XCTAssertFalse(UnpeelStore.shouldDisplayHostProjection(
            scope: .local,
            localClientStarted: false,
            localProjectionReady: false
        ))
        XCTAssertFalse(UnpeelStore.shouldDisplayHostProjection(
            scope: .local,
            localClientStarted: true,
            localProjectionReady: false
        ))
        XCTAssertTrue(UnpeelStore.shouldDisplayHostProjection(
            scope: .local,
            localClientStarted: true,
            localProjectionReady: true
        ))
        XCTAssertTrue(UnpeelStore.shouldDisplayHostProjection(
            scope: .remote(hostID: "host"),
            localClientStarted: false,
            localProjectionReady: false
        ))
    }

    func testLocalClientRoutesVerbsThroughHostAndFailsClosedDuringRecovery() {
        XCTAssertFalse(UnpeelStore.shouldRouteHostVerb(
            scope: .local,
            localClientStarted: false,
            projectedEntityExists: true
        ))
        XCTAssertTrue(UnpeelStore.shouldRouteHostVerb(
            scope: .local,
            localClientStarted: true,
            projectedEntityExists: false
        ))
        XCTAssertTrue(UnpeelStore.shouldRouteHostVerb(
            scope: .localWorkspace(home: "/tmp/work", name: "Work"),
            localClientStarted: false,
            projectedEntityExists: true
        ))
        XCTAssertTrue(UnpeelStore.shouldRouteHostVerb(
            scope: .remote(hostID: "host"),
            localClientStarted: false,
            projectedEntityExists: true
        ))
        XCTAssertFalse(UnpeelStore.shouldRouteHostVerb(
            scope: .remote(hostID: "host"),
            localClientStarted: true,
            projectedEntityExists: false
        ))
    }

    func testLegacyProjectTombstoneRemovesTheWholeProjectedSubtree() {
        XCTAssertEqual(
            UnpeelStore.projectSubtreeIDs(
                roots: ["clarity"],
                parentByProjectID: [
                    "pinned": "clarity",
                    "nested": "pinned",
                    "other-child": "other",
                ]
            ),
            ["clarity", "pinned", "nested"]
        )
    }

    func testDiskProjectionStopsAfterFirstCompleteLocalHostSnapshot() {
        XCTAssertTrue(UnpeelStore.shouldApplyLocalDiskProjection(
            localHostClientStarted: false,
            localHostProjectionReady: false
        ))
        XCTAssertTrue(UnpeelStore.shouldApplyLocalDiskProjection(
            localHostClientStarted: true,
            localHostProjectionReady: false
        ))
        XCTAssertFalse(UnpeelStore.shouldApplyLocalDiskProjection(
            localHostClientStarted: true,
            localHostProjectionReady: true
        ))
    }
}
