import XCTest
@testable import UnpeelShared

final class PairedHostRecordTests: XCTestCase {
    func testPairingDTOBecomesHostNamedControllerState() throws {
        let response = RemotePairingResponse(
            macID: "headless-1",
            macName: "Studio Server",
            endpoint: try XCTUnwrap(URL(string: "http://10.0.0.4:17661/mobile")),
            deviceID: "controller-1",
            authToken: "secret",
            pairedAtUnixMs: 42,
            remoteServerPort: 2048,
            remoteServerCertificateFingerprint: "secure-pin",
            relayCredentials: RelayCredentials(
                relayURL: try XCTUnwrap(URL(string: "wss://relay.example")),
                macID: "headless-1",
                relayToken: "relay-token",
                e2eKey: Data(repeating: 7, count: 32)
            )
        )

        let record = PairedHostRecord(pairing: response, certificateFingerprint: "pair-pin")

        XCTAssertEqual(record.id, "headless-1")
        XCTAssertEqual(record.hostID, response.macID)
        XCTAssertEqual(record.name, "Studio Server")
        XCTAssertEqual(record.certificateFingerprint, "pair-pin")
        XCTAssertEqual(record.remoteServerPort, 2048)
        XCTAssertEqual(record.remoteServerCertificateFingerprint, "secure-pin")
    }

    /// Records persisted by builds that predate the per-Host Link flag have
    /// no `linkEnabled` key — they must decode as allowed (nil), and the
    /// flag must follow the narrows-only convention.
    func testStoredRecordWithoutLinkEnabledDecodesAsAllowed() throws {
        let legacyJSON = Data("""
        {
            "hostID": "headless-1",
            "name": "Studio Server",
            "endpoint": "http://10.0.0.4:17661/mobile",
            "controllerDeviceID": "controller-1",
            "pairedAtUnixMs": 42
        }
        """.utf8)

        let record = try JSONDecoder().decode(PairedHostRecord.self, from: legacyJSON)

        XCTAssertNil(record.linkEnabled)
        XCTAssertTrue(record.isLinkEnabled)

        var narrowed = record
        narrowed.linkEnabled = false
        XCTAssertFalse(narrowed.isLinkEnabled)
        let reloaded = try JSONDecoder().decode(
            PairedHostRecord.self,
            from: JSONEncoder().encode(narrowed)
        )
        XCTAssertEqual(reloaded.linkEnabled, false)
    }

    func testUpsertPreservesPickerOrder() throws {
        func record(_ id: String, _ name: String) throws -> PairedHostRecord {
            PairedHostRecord(
                hostID: id,
                name: name,
                endpoint: try XCTUnwrap(URL(string: "http://127.0.0.1:17661/mobile")),
                controllerDeviceID: "controller",
                pairedAtUnixMs: 1
            )
        }
        let original = [try record("a", "A"), try record("b", "B")]

        let updated = PairedHostCollection.upserting(original, with: try record("a", "A2"))
        let appended = PairedHostCollection.upserting(updated, with: try record("c", "C"))

        XCTAssertEqual(updated.map(\.hostID), ["a", "b"])
        XCTAssertEqual(updated.first?.name, "A2")
        XCTAssertEqual(appended.map(\.hostID), ["a", "b", "c"])
        XCTAssertEqual(PairedHostCollection.removing(appended, hostID: "b").map(\.hostID), ["a", "c"])
    }
}
