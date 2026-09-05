import XCTest
@testable import UnpeelShared

final class RuntimeCatalogTests: XCTestCase {
    private func runtimeFixture(
        slug: String,
        platform: UnpeelRuntimePlatform,
        kind: UnpeelRuntimeKind = .agent
    ) -> UnpeelRuntimeMetadata {
        UnpeelRuntimeMetadata(
            stableID: "com.example.\(slug)",
            slug: slug,
            legacySlug: slug,
            legacyOrder: nil,
            label: slug,
            platforms: [platform],
            supportsQuickLaunch: true,
            kind: kind,
            tintColorHex: nil,
            spinnerTintColorHex: nil,
            iconKey: "agent",
            iconSVG: nil,
            iconIsTemplate: true,
            iconSource: nil,
            iconLicense: nil,
            windowPaddingX: 0,
            installURL: "https://example.com/\(slug)",
            installCommand: "install \(slug)",
            commandAliases: [slug],
            processAliases: [slug],
            searchPathSuffixes: [".\(slug)/bin"],
            lifecycleSource: "output",
            lifecycleAuthority: "none",
            lifecycleFallback: "none",
            completionReliable: false,
            attentionReliable: false,
            anchorStartEventToOutput: true,
            attentionClearsOnOutput: true,
            distrustStopsWhileOutputGrows: false,
            capabilities: [],
            usageStores: [],
            suggestedPresets: [
                UnpeelRuntimeSuggestedPreset(
                    id: "\(slug)-default",
                    label: slug,
                    command: slug,
                    quickLaunch: true
                ),
            ]
        )
    }

    func testGeneratedCatalogPreservesLegacyOrderAndStableIdentity() throws {
        let orders = UnpeelRuntimeCatalog.runtimes.compactMap(\.legacyOrder)
        XCTAssertEqual(orders, Array(0 ..< orders.count))

        let claude = try XCTUnwrap(
            UnpeelRuntimeCatalog.runtime(id: "com.anthropic.claude-code")
        )
        XCTAssertEqual(claude.legacySlug, "claude")
        XCTAssertEqual(claude.defaultPreset?.command, "claude")
        XCTAssertEqual(claude.kind, .agent)
        XCTAssertEqual(claude.tintColorHex, 0xD97757)
        XCTAssertEqual(claude.installCommand, "npm install -g @anthropic-ai/claude-code")
        XCTAssertEqual(claude.lifecycleSource, "hooks")
        XCTAssertTrue(claude.capabilities.contains(.restartAgent))
        XCTAssertTrue(claude.capabilities.contains(.transcript))
        XCTAssertEqual(claude.usageStores.first?.root, ".claude/projects")
        XCTAssertEqual(claude.windowPaddingX, 8)

        let edgeToEdgeRuntimeIDs: Set<String> = ["ai.x.grok-cli", "ai.opencode.cli"]
        for runtime in UnpeelRuntimeCatalog.runtimes where runtime.kind == .agent {
            XCTAssertEqual(
                runtime.windowPaddingX,
                edgeToEdgeRuntimeIDs.contains(runtime.stableID) ? 0 : 8,
                runtime.stableID
            )
        }

        let kimi = try XCTUnwrap(UnpeelRuntimeCatalog.runtime(id: "kimi"))
        XCTAssertEqual(kimi.searchPathSuffixes, [".kimi-code/bin"])
        XCTAssertEqual(kimi.usageStores.count, 2)
    }

    func testCommandLookupUsesDescriptorAliasesAndExecutableBasename() {
        XCTAssertEqual(
            UnpeelRuntimeCatalog.runtime(command: "/opt/homebrew/bin/codex --full-auto")?.stableID,
            "com.openai.codex"
        )
        XCTAssertNil(UnpeelRuntimeCatalog.runtime(command: "npm run dev"))
    }

    func testPlatformFilteringUsesMacOSAndLinuxDescriptorFixtures() {
        let macOS = runtimeFixture(slug: "macos-only", platform: .macos)
        let linux = runtimeFixture(slug: "linux-only", platform: .linux)
        let fixtures = [linux, macOS]

        XCTAssertTrue(macOS.supports(.macos))
        XCTAssertFalse(macOS.supports(.linux))
        XCTAssertTrue(linux.supports(.linux))
        XCTAssertFalse(linux.supports(.macos))
        XCTAssertEqual(
            UnpeelRuntimeCatalog.runtimes(for: .macos, from: fixtures).map(\.slug),
            ["macos-only"]
        )
        XCTAssertEqual(
            UnpeelRuntimeCatalog.runtimes(for: .linux, from: fixtures).map(\.slug),
            ["linux-only"]
        )

        // Presentation/icon resolution remains platform-neutral for a remote
        // Host's runtime, even when it cannot be launched on this client Mac.
        XCTAssertEqual(UnpeelToolIcon.forRuntime(linux).id, "com.example.linux-only")
    }

    func testSharedIconResolutionPrefersHostProviderIdentity() {
        let icon = UnpeelToolIcon.resolving(
            providerID: "com.anthropic.claude-code",
            command: "codex"
        )
        XCTAssertEqual(icon.id, "com.anthropic.claude-code")
        XCTAssertEqual(icon.key, "claude")
        XCTAssertTrue(icon.usesRuntimeAsset)
        XCTAssertEqual(
            UnpeelToolIcon.resolving(providerID: nil, command: "unknown-agent"),
            .terminal
        )
    }

    func testGenericFallbackFollowsRuntimeKindNotAgentSparkle() {
        let editor = UnpeelToolIcon.forRuntime(
            runtimeFixture(slug: "markdown", platform: .macos, kind: .editor)
        )
        let agent = UnpeelToolIcon.forRuntime(
            runtimeFixture(slug: "agent", platform: .macos, kind: .agent)
        )
        XCTAssertEqual(editor.kind, .editor)
        XCTAssertEqual(editor.fallbackSystemName, "doc.plaintext")
        XCTAssertFalse(editor.usesRuntimeAsset)
        XCTAssertNotEqual(editor.svgSource, agent.svgSource)
    }

    func testEveryRuntimeDescriptorResolvesAnEmbeddedOrGenericIcon() throws {
        for runtime in UnpeelRuntimeCatalog.runtimes {
            let icon = UnpeelToolIcon.forRuntime(runtime)
            XCTAssertEqual(icon.id, runtime.stableID, runtime.slug)
            XCTAssertEqual(icon.key, runtime.iconKey, runtime.slug)
            XCTAssertFalse(icon.svgSource.isEmpty, runtime.slug)

            if runtime.iconSVG == nil {
                XCTAssertFalse(icon.usesRuntimeAsset, runtime.slug)
                XCTAssertTrue(icon.isTemplate, runtime.slug)
                XCTAssertNil(runtime.iconSource, runtime.slug)
                XCTAssertNil(runtime.iconLicense, runtime.slug)
            } else {
                XCTAssertTrue(icon.usesRuntimeAsset, runtime.slug)
                XCTAssertNotNil(runtime.iconSource, runtime.slug)
                XCTAssertNotNil(runtime.iconLicense, runtime.slug)
                XCTAssertEqual(icon.isTemplate, runtime.iconIsTemplate, runtime.slug)
            }
        }

        let copilot = try XCTUnwrap(UnpeelRuntimeCatalog.runtime(id: "com.github.copilot-cli"))
        XCTAssertNil(copilot.iconSVG)
        XCTAssertFalse(UnpeelToolIcon.forRuntime(copilot).usesRuntimeAsset)

        let openCode = try XCTUnwrap(UnpeelRuntimeCatalog.runtime(id: "ai.opencode.cli"))
        XCTAssertNotNil(openCode.iconSVG)
        XCTAssertFalse(openCode.iconIsTemplate)
        XCTAssertFalse(UnpeelToolIcon.forRuntime(openCode).isTemplate)
    }
}
