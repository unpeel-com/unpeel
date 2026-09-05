import XCTest
@testable import UnpeelNative

final class OpenCodeThemeTests: XCTestCase {
    func testAuraUsesOpenCodeCanvasBackground() throws {
        let background = try resolvedBackground(forTheme: "aura")

        XCTAssertEqual(background.light, 0x0F0F0F)
        XCTAssertEqual(background.dark, 0x0F0F0F)
    }

    func testDefaultOpenCodeBackgroundMatchesBuiltInTheme() throws {
        let background = try resolvedBackground(forTheme: "opencode")

        XCTAssertEqual(background.light, 0xFFFFFF)
        XCTAssertEqual(background.dark, 0x0A0A0A)
    }

    func testSingleModeOpenCodeThemesDoNotInventLightBackgrounds() throws {
        let background = try resolvedBackground(forTheme: "catppuccin-frappe")

        XCTAssertEqual(background.light, 0x303446)
        XCTAssertEqual(background.dark, 0x303446)
    }

    func testDefaultGrokNightBackgroundMatchesFrameColor() throws {
        let background = try resolvedGrokBackground("""
        [ui]
        theme = "auto"
        """)

        XCTAssertEqual(background.light, 0xFAFAFA)
        // Live Grok 0.2.x paints truecolor canvas rgb(20,20,20).
        XCTAssertEqual(background.dark, 0x141414)
    }

    func testGrokFixedDarkThemeUsesCanvasBackground() throws {
        let background = try resolvedGrokBackground("""
        [ui]
        theme = "groknight"
        """)

        XCTAssertEqual(background.light, 0x141414)
        XCTAssertEqual(background.dark, 0x141414)
    }

    func testProviderThemeWatchPathsRecognizeConfigFiles() {
        XCTAssertTrue(
            ProviderThemeWatchPaths.isRelevantChange(
                "/Users/me/.grok/config.toml"
            )
        )
        XCTAssertTrue(
            ProviderThemeWatchPaths.isRelevantChange(
                "/Users/me/.config/opencode/opencode.json"
            )
        )
        XCTAssertTrue(
            ProviderThemeWatchPaths.isRelevantChange(
                "/Users/me/proj/.opencode/themes/aura.json"
            )
        )
        XCTAssertTrue(
            ProviderThemeWatchPaths.isRelevantChange(
                "/Users/me/.local/state/opencode/kv.json"
            )
        )
        XCTAssertFalse(
            ProviderThemeWatchPaths.isRelevantChange(
                "/Users/me/.grok/sessions/foo/updates.jsonl"
            )
        )
        XCTAssertFalse(
            ProviderThemeWatchPaths.isRelevantChange(
                "/Users/me/proj/src/main.swift"
            )
        )
    }

    func testGrokAndOpenCodeCommandsUseProviderTheme() {
        XCTAssertTrue(TerminalFrameStyle.usesProviderTheme(command: "grok --always-approve"))
        XCTAssertTrue(TerminalFrameStyle.usesProviderTheme(command: "opencode"))
        XCTAssertFalse(TerminalFrameStyle.usesProviderTheme(command: "claude --dangerously-skip-permissions"))
    }

    func testCanvasSamplerPicksDominantTruecolorBackground() {
        // Simulate Grok-style SGR: many cells with canvas #141414, a few
        // chrome cells with #111111 — sampler should lock onto the canvas.
        var payload = Data()
        let canvas = "\u{1B}[48;2;20;20;20m "
        let chrome = "\u{1B}[48;2;17;17;17m "
        for _ in 0 ..< 80 {
            payload.append(contentsOf: canvas.utf8)
        }
        for _ in 0 ..< 10 {
            payload.append(contentsOf: chrome.utf8)
        }
        // Compound form Grok often uses: 38;2;…;48;2;R;G;B
        let compound = "\u{1B}[38;2;200;200;200;48;2;20;20;20mX"
        for _ in 0 ..< 40 {
            payload.append(contentsOf: compound.utf8)
        }

        XCTAssertEqual(ProviderCanvasSampler.dominantBackground(in: payload), 0x141414)
    }

    func testCanvasSamplerRejectsAmbiguousBackgrounds() {
        var payload = Data()
        let a = "\u{1B}[48;2;20;20;20m "
        let b = "\u{1B}[48;2;40;40;40m "
        for _ in 0 ..< 50 {
            payload.append(contentsOf: a.utf8)
            payload.append(contentsOf: b.utf8)
        }
        XCTAssertNil(ProviderCanvasSampler.dominantBackground(in: payload))
    }

    func testFixedBackgroundOverrideWinsOverConfig() throws {
        let base = try resolvedGrokBackground("""
        [ui]
        theme = "groknight"
        """)
        XCTAssertEqual(base.dark, 0x141414)

        let style = TerminalFrameStyle.resolved(
            command: "grok --always-approve",
            workingDirectory: nil,
            canvasOverride: 0x0A0A12
        )
        XCTAssertEqual(style.background?.dark, 0x0A0A12)
        XCTAssertEqual(style.background?.light, 0x0A0A12)
        XCTAssertEqual(style.paneStyle.dark.background.uppercased(), "#0A0A12")
    }

    func testRuntimeDescriptorControlsHorizontalPanePadding() {
        XCTAssertEqual(
            TerminalFrameStyle.resolved(
                command: "claude --session-id example",
                workingDirectory: nil
            ).paneStyle.windowPaddingX,
            8
        )
        XCTAssertEqual(
            TerminalFrameStyle.resolved(
                command: "codex",
                workingDirectory: nil
            ).paneStyle.windowPaddingX,
            8
        )
        XCTAssertEqual(
            TerminalFrameStyle.resolved(
                command: "/bin/zsh --login",
                workingDirectory: nil
            ).paneStyle.windowPaddingX,
            0
        )
        XCTAssertEqual(
            TerminalPaneStyle.resolved(
                runtimeID: "com.anthropic.claude-code",
                command: "codex"
            ).windowPaddingX,
            8
        )
        XCTAssertEqual(
            TerminalFrameStyle.resolved(
                command: "grok --always-approve",
                workingDirectory: nil
            ).paneStyle.windowPaddingX,
            0
        )
        XCTAssertEqual(
            TerminalFrameStyle.resolved(
                command: "opencode",
                workingDirectory: nil
            ).paneStyle.windowPaddingX,
            0
        )
    }

    private func resolvedBackground(forTheme theme: String) throws -> TerminalFrameStyle.Background {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let configDirectory = directory.appendingPathComponent(".opencode", isDirectory: true)
        try FileManager.default.createDirectory(
            at: configDirectory,
            withIntermediateDirectories: true
        )
        try Data(#"{"theme":"\#(theme)"}"#.utf8).write(
            to: configDirectory.appendingPathComponent("tui.json")
        )
        addTeardownBlock {
            try? FileManager.default.removeItem(at: directory)
        }

        return try XCTUnwrap(OpenCodeThemeResolver.background(workingDirectory: directory.path))
    }

    private func resolvedGrokBackground(_ config: String) throws -> TerminalFrameStyle.Background {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        try Data(config.utf8).write(to: directory.appendingPathComponent("config.toml"))

        let oldGrokHome = getenv("GROK_HOME").map { String(cString: $0) }
        setenv("GROK_HOME", directory.path, 1)
        addTeardownBlock {
            if let oldGrokHome {
                setenv("GROK_HOME", oldGrokHome, 1)
            } else {
                unsetenv("GROK_HOME")
            }
            try? FileManager.default.removeItem(at: directory)
        }

        return try XCTUnwrap(GrokThemeResolver.background(command: "grok --always-approve"))
    }
}
