import XCTest
@testable import UnpeelIOS

final class PushManagerTests: XCTestCase {
    func testRegistrationDiagnosticsNamePermissionTokenAndEnvironmentStates() {
        XCTAssertEqual(
            PushRegistrationState.permissionDenied.diagnosticLabel,
            "Notifications are denied in iOS Settings"
        )
        XCTAssertEqual(
            PushRegistrationState.registering.diagnosticLabel,
            "Waiting for an APNs device token…"
        )
        XCTAssertEqual(
            PushRegistrationState.registered(environment: "production").diagnosticLabel,
            "Ready (production)"
        )
        XCTAssertTrue(PushRegistrationState.permissionDenied.permissionWasDenied)
        XCTAssertTrue(PushRegistrationState.failed("offline").canRetry)
        XCTAssertFalse(PushRegistrationState.registering.canRetry)
    }
}
