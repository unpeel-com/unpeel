import XCTest
@testable import UnpeelNative

final class RemoteContentBannerPolicyTests: XCTestCase {
    func testRetainedConnectingStateWaitsForDelay() {
        XCTAssertTrue(RemoteContentBannerPolicy.shouldScheduleConnectingDelay(
            state: .connecting,
            hasSnapshot: true,
            isRemoteScope: true
        ))
        XCTAssertFalse(RemoteContentBannerPolicy.allowsContentBanner(
            state: .connecting,
            isRemoteScope: true,
            connectingDelayElapsed: false
        ))
        XCTAssertTrue(RemoteContentBannerPolicy.allowsContentBanner(
            state: .connecting,
            isRemoteScope: true,
            connectingDelayElapsed: true
        ))
    }

    func testInstantOrSnapshotlessConnectionNeverSchedulesStaleBanner() {
        XCTAssertFalse(RemoteContentBannerPolicy.shouldScheduleConnectingDelay(
            state: .connecting,
            hasSnapshot: false,
            isRemoteScope: true
        ))
        XCTAssertFalse(RemoteContentBannerPolicy.shouldScheduleConnectingDelay(
            state: .connected(name: "Host"),
            hasSnapshot: true,
            isRemoteScope: true
        ))
        XCTAssertFalse(RemoteContentBannerPolicy.shouldScheduleConnectingDelay(
            state: .connecting,
            hasSnapshot: true,
            isRemoteScope: false
        ))
    }

    func testReconnectAndErrorsBypassUnexpiredConnectingDelay() {
        let immediateStates: [RemoteHostConnectionState] = [
            .reconnecting(message: "retrying"),
            .repairRequired(message: "pair again"),
            .incompatible(message: "update required"),
            .failed(message: "offline")
        ]

        for state in immediateStates {
            XCTAssertTrue(RemoteContentBannerPolicy.allowsContentBanner(
                state: state,
                isRemoteScope: true,
                connectingDelayElapsed: false
            ))
        }
    }
}
