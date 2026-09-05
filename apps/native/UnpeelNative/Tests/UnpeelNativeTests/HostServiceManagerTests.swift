import XCTest
@testable import UnpeelNative

final class HostServiceManagerTests: XCTestCase {
    func testLaunchRetryIsBoundedButEventuallyRearms() {
        let first = Date(timeIntervalSince1970: 100)
        XCTAssertTrue(HostServiceManager.shouldAttemptLaunch(
            now: first,
            lastAttemptAt: nil,
            cooldown: 5
        ))
        XCTAssertFalse(HostServiceManager.shouldAttemptLaunch(
            now: first.addingTimeInterval(4.99),
            lastAttemptAt: first,
            cooldown: 5
        ))
        XCTAssertTrue(HostServiceManager.shouldAttemptLaunch(
            now: first.addingTimeInterval(5),
            lastAttemptAt: first,
            cooldown: 5
        ))
    }

    func testPlatformAdapterTokenMeetsRegistrationBoundary() {
        let first = HostServiceManager.platformAdapterToken()
        let second = HostServiceManager.platformAdapterToken()
        XCTAssertEqual(first.utf8.count, 64)
        XCTAssertNotEqual(first, second)
        XCTAssertTrue(first.utf8.allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        })
    }
}
