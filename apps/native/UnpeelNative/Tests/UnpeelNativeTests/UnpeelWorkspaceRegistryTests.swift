import XCTest
@testable import UnpeelNative

final class UnpeelWorkspaceRegistryTests: XCTestCase {
    func testDecodesReleasedProfilesRegistryWithoutChangingHomes() throws {
        let data = Data(
            #"{"version":1,"profiles":[{"id":"work","name":"Work","home":"/Users/test/.unpeel/profiles/work","createdAt":1234}]}"#.utf8
        )

        XCTAssertEqual(
            try UnpeelWorkspaceRegistry.decodeRegistry(data),
            [
                UnpeelWorkspaceRecord(
                    id: "work",
                    name: "Work",
                    home: "/Users/test/.unpeel/profiles/work",
                    createdAt: 1234
                )
            ]
        )
    }

    func testEncodesReleasedProfilesKeyInsteadOfNewWorkspaceKey() throws {
        let record = UnpeelWorkspaceRecord(
            id: "studio",
            name: "Studio",
            home: "/Users/test/.unpeel/profiles/studio",
            createdAt: 5678
        )
        let data = try UnpeelWorkspaceRegistry.encodeRegistry([record])
        let root = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        let stored = try XCTUnwrap(root["profiles"] as? [[String: Any]])

        XCTAssertEqual(root["version"] as? Int, 1)
        XCTAssertNil(root["workspaces"])
        XCTAssertEqual(stored.count, 1)
        XCTAssertEqual(stored[0]["id"] as? String, record.id)
        XCTAssertEqual(stored[0]["name"] as? String, record.name)
        XCTAssertEqual(stored[0]["home"] as? String, record.home)
        XCTAssertEqual(stored[0]["createdAt"] as? Int, Int(record.createdAt))
    }

    func testLegacyRegistryLocationsAndNewSlugFallbackStayExplicit() {
        XCTAssertTrue(
            UnpeelWorkspaceRegistry.registryURL.path.hasSuffix("/.unpeel/profiles.json")
        )
        XCTAssertTrue(
            UnpeelWorkspaceRegistry.workspaceHomesRoot.path.hasSuffix("/.unpeel/profiles")
        )
        XCTAssertEqual(UnpeelWorkspaceRegistry.slugify("✨"), "workspace")
        XCTAssertEqual(UnpeelWorkspaceRegistry.slugify("Client Work"), "client-work")
    }

    func testWorkspaceFeatureRetainsReleasedPreferenceAndLegacyEnvAlias() {
        let feature = ExperimentalFeature.workspaces

        XCTAssertEqual(feature.key, "profiles")
        XCTAssertEqual(feature.defaultsKey, "unpeel.experimental.profiles")
        XCTAssertEqual(feature.envOverride, "UNPEEL_DEV_WORKSPACES")
        XCTAssertEqual(feature.legacyEnvOverrides, ["UNPEEL_DEV_PROFILES"])
        XCTAssertEqual(
            feature.envOverrides,
            ["UNPEEL_DEV_WORKSPACES", "UNPEEL_DEV_PROFILES"]
        )
    }

    func testLocalWorkspacePickerDoesNotDependOnRemoteDevelopmentGate() {
        XCTAssertTrue(WorkspaceFeature.pickerEnabled(
            localWorkspacesEnabled: true,
            remoteHostPickerEnabled: false
        ))
        XCTAssertTrue(WorkspaceFeature.pickerEnabled(
            localWorkspacesEnabled: false,
            remoteHostPickerEnabled: true
        ))
        XCTAssertFalse(WorkspaceFeature.pickerEnabled(
            localWorkspacesEnabled: false,
            remoteHostPickerEnabled: false
        ))
    }

    func testLegacyProfilesSettingsDeepLinkOpensWorkspaces() {
        XCTAssertEqual(SettingsTab.compatibleRawValue("profiles"), .workspaces)
        XCTAssertEqual(SettingsTab.compatibleRawValue("workspaces"), .workspaces)
        XCTAssertEqual(SettingsTab.compatibleRawValue("advanced"), .advanced)
    }

    func testWorktreesSettingsTab() {
        XCTAssertEqual(SettingsTab.compatibleRawValue("worktrees"), .worktrees)
        XCTAssertEqual(SettingsTab.worktrees.title, "Worktrees")
        XCTAssertFalse(SettingsTab.hostScopedCases.contains(.worktrees))
        XCTAssertFalse(SettingsTab.worktrees.isBuiltInMCP)
    }
}
