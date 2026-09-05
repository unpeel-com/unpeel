import XCTest
@testable import UnpeelNative

final class NearbyHostBrowserTests: XCTestCase {
    func testAdvertisementRequiresStableHostIdentity() {
        XCTAssertNil(NearbyHostCatalog.candidate(serviceName: "Studio", txt: [:]))
        XCTAssertNil(NearbyHostCatalog.candidate(serviceName: "Studio", txt: ["macid": " "]))
        XCTAssertEqual(
            NearbyHostCatalog.candidate(
                serviceName: "  Studio Mac  ",
                txt: ["macid": "host-1"]
            ),
            NearbyHostCandidate(hostID: "host-1", name: "Studio Mac")
        )
    }

    func testCatalogDeduplicatesHostIdentityAndSortsForPicker() {
        let merged = NearbyHostCatalog.merging([
            NearbyHostCandidate(hostID: "b", name: "Zulu"),
            NearbyHostCandidate(hostID: "a", name: "Alpha"),
            NearbyHostCandidate(hostID: "a", name: "Spoofed duplicate"),
        ])

        XCTAssertEqual(merged.map(\.hostID), ["a", "b"])
        XCTAssertEqual(merged.map(\.name), ["Alpha", "Zulu"])
    }

    func testCatalogExcludesOnlyThisLogicalHost() {
        let merged = NearbyHostCatalog.merging(
            [
                NearbyHostCandidate(hostID: "CURRENT-HOST", name: "This Mac"),
                NearbyHostCandidate(hostID: "other-workspace", name: "Other Workspace"),
                NearbyHostCandidate(hostID: "remote-host", name: "Remote Mac"),
            ],
            excludingHostID: "current-host"
        )

        XCTAssertEqual(merged.map(\.hostID), ["other-workspace", "remote-host"])
    }
}
