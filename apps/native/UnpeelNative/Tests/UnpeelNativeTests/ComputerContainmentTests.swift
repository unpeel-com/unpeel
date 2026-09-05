import XCTest
@testable import UnpeelNative

final class ComputerContainmentTests: XCTestCase {
    func testComputerUseDefaultsOff() {
        XCTAssertFalse(ExperimentalFeature.computerUse.defaultOn)
    }

    func testComputerUseRequiresBooleanDevelopmentBuildMarker() {
        XCTAssertFalse(UnpeelFeatureFlags.computerUseAvailable(infoDictionary: nil))
        XCTAssertFalse(UnpeelFeatureFlags.computerUseAvailable(infoDictionary: [:]))
        XCTAssertFalse(UnpeelFeatureFlags.computerUseAvailable(
            infoDictionary: ["UnpeelDevelopmentBuild": false]
        ))
        XCTAssertFalse(UnpeelFeatureFlags.computerUseAvailable(
            infoDictionary: ["UnpeelDevelopmentBuild": "true"]
        ))
        XCTAssertTrue(UnpeelFeatureFlags.computerUseAvailable(
            infoDictionary: ["UnpeelDevelopmentBuild": true]
        ))
    }

    func testProductionAvailabilityExcludesOnlyComputerUse() {
        XCTAssertFalse(UnpeelFeatureFlags.isAvailable(
            .computerUse, developmentBuild: false
        ))
        XCTAssertTrue(UnpeelFeatureFlags.isAvailable(
            .computerUse, developmentBuild: true
        ))

        for feature in ExperimentalFeature.all where feature != .computerUse {
            XCTAssertTrue(
                UnpeelFeatureFlags.isAvailable(feature, developmentBuild: false),
                "production unexpectedly hid \(feature.key)"
            )
        }
    }

    /// Decision D2: operating a Host's computer use follows what that Host
    /// advertises, never this app's build flavor.
    func testControllableFollowsTheHostAdvertisementNotTheBuild() {
        XCTAssertTrue(UnpeelFeatureFlags.computerUseControllable(hostAdvertisesAvailability: true))
        XCTAssertFalse(UnpeelFeatureFlags.computerUseControllable(hostAdvertisesAvailability: false))
        XCTAssertFalse(UnpeelFeatureFlags.computerUseControllable(hostAdvertisesAvailability: nil))
    }

    func testReleaseBuildShowsTheComputerTabOnlyForAnAdvertisingHost() {
        // Release build + advertising (Linux) Host → the tab is reachable.
        XCTAssertTrue(SettingsTab.visibleCases(computerUseControllable: true).contains(.computer))
        // Release build + this Mac's local scope → hidden, exactly as today.
        // (`visibleCases` without the Host flag is the local-scope path; the
        // test runner is not a development bundle.)
        XCTAssertEqual(
            SettingsTab.visibleCases(computerUseControllable: false).contains(.computer),
            UnpeelFeatureFlags.isAvailable(.computerUse)
        )
        XCTAssertEqual(
            SettingsTab.visibleCases.contains(.computer),
            SettingsTab.visibleCases(computerUseControllable: false).contains(.computer)
        )
    }

    func testComputerUseExperimentWriteKeepsSiblingGatesAndUnknownKeys() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-cu-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("app-state.json")
        let seed: [String: Any] = [
            "projects": [], "presets": [],
            "experimental_features": ["sessions_mcp": true, "future_gate": "keep"],
            "a_key_from_a_future_version": ["nested": [1, 2, 3]],
        ]
        try JSONSerialization.data(withJSONObject: seed).write(to: file)

        XCTAssertTrue(UnpeelStore.writeComputerUseExperiment(true, appStateFile: file))
        let after = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]
        )
        let features = try XCTUnwrap(after["experimental_features"] as? [String: Any])
        XCTAssertEqual(features["computer_use"] as? Bool, true)
        XCTAssertEqual(features["sessions_mcp"] as? Bool, true)
        XCTAssertEqual(features["future_gate"] as? String, "keep")
        XCTAssertNotNil(after["a_key_from_a_future_version"])

        XCTAssertTrue(UnpeelStore.writeComputerUseExperiment(false, appStateFile: file))
        let off = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]
        )
        XCTAssertEqual((off["experimental_features"] as? [String: Any])?["computer_use"] as? Bool, false)
    }
}
