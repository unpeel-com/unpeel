import XCTest
@testable import UnpeelNative

final class SelectedHostScopeTests: XCTestCase {
    func testSessionPanesAreAControllerCapabilityInEveryScope() {
        let secondaryWorkspace = SelectedHostScope.localWorkspace(
            home: "/Users/me/.unpeel/profiles/writing",
            name: "Writing"
        )

        XCTAssertTrue(SelectedHostScope.local.supportsSessionPanes)
        XCTAssertTrue(secondaryWorkspace.supportsSessionPanes)
        XCTAssertFalse(secondaryWorkspace.permitsLocalExecution)
        XCTAssertTrue(
            SelectedHostScope.remote(hostID: "studio-mac").supportsSessionPanes
        )
        XCTAssertEqual(SelectedHostScope.local.paneScopeID, "local")
        XCTAssertEqual(
            secondaryWorkspace.paneScopeID,
            SelectedHostScope.localWorkspace(
                home: "/Users/me/.unpeel/profiles/writing",
                name: "Renamed Writing"
            ).paneScopeID
        )
        XCTAssertNotEqual(
            secondaryWorkspace.paneScopeID,
            SelectedHostScope.remote(hostID: "studio-mac").paneScopeID
        )
    }

    func testRemoteScopeIsNeverThisInstancesHome() {
        let scope = SelectedHostScope.remote(hostID: "studio-mac")

        XCTAssertFalse(scope.permitsLocalExecution)
        XCTAssertTrue(SelectedHostScope.local.permitsLocalExecution)
        XCTAssertEqual(scope.sessionLaunchWireValue, "remote_controller")
        XCTAssertEqual(scope.remoteHostID, "studio-mac")
        XCTAssertNil(SelectedHostScope.local.remoteHostID)
    }

    /// A workspace scoped over the loopback gateway is NOT this instance's
    /// home: it must hit the exact same refusals as a remote Host scope.
    func testLocalWorkspaceScopeIsRemoteLikeAtEveryChokePoint() {
        let scope = SelectedHostScope.localWorkspace(
            home: "/Users/me/.unpeel/profiles/writing",
            name: "Writing"
        )

        XCTAssertFalse(scope.permitsLocalExecution)
        XCTAssertEqual(scope.sessionLaunchWireValue, "remote_controller")
        XCTAssertNil(scope.remoteHostID)
        XCTAssertEqual(scope.localWorkspaceHome, "/Users/me/.unpeel/profiles/writing")
        XCTAssertEqual(scope.localWorkspaceName, "Writing")
        XCTAssertNil(SelectedHostScope.local.localWorkspaceHome)
        XCTAssertNil(SelectedHostScope.remote(hostID: "studio-mac").localWorkspaceHome)
    }
}
