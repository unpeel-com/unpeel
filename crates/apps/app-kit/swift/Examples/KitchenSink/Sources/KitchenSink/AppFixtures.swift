import Foundation

extension DemoKind {
    func kitchenLaunchDirectory(sessionDirectory: String) -> String {
        isCrossPlatformAuditApp
            ? URL(fileURLWithPath: sessionDirectory, isDirectory: true)
                .appendingPathComponent("workspace", isDirectory: true).path
            : sessionDirectory
    }

    func kitchenFixtureEnvironment(sessionDirectory: String) -> [String: String] {
        let root = URL(fileURLWithPath: sessionDirectory, isDirectory: true)
        switch self {
        case .usageApp:
            return [
                "HOME": root.appendingPathComponent("home", isDirectory: true).path,
                "XDG_CONFIG_HOME": root.appendingPathComponent("config", isDirectory: true).path,
                "XDG_DATA_HOME": root.appendingPathComponent("data", isDirectory: true).path,
                "CLAUDE_CONFIG_DIR": root.appendingPathComponent("home/.claude").path,
                "GROK_HOME": root.appendingPathComponent("home/.grok").path,
            ]
        case .githubIssuesApp:
            return [
                "PATH": root.appendingPathComponent("bin", isDirectory: true).path
                    + ":/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
            ]
        case .markdownApp:
            return [
                "UNPEEL_APP_CONFIG_HOME":
                    root
                    .appendingPathComponent("config/unpeel-apps", isDirectory: true).path
            ]
        case .diffsApp, .filetreeApp, .charts, .todo, .markdown, .media, .surface, .canvas:
            return [:]
        }
    }

    func prepareKitchenFixture(sessionDirectory: String) throws {
        guard isCrossPlatformAuditApp else { return }
        let root = URL(fileURLWithPath: sessionDirectory, isDirectory: true)
        let workspace = root.appendingPathComponent("workspace", isDirectory: true)
        try FileManager.default.createDirectory(
            at: workspace,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        switch self {
        case .usageApp:
            try prepareUsageFixture(root: root, workspace: workspace)
        case .diffsApp:
            try prepareDiffsFixture(workspace: workspace)
        case .githubIssuesApp:
            try prepareIssuesFixture(root: root, workspace: workspace)
        case .markdownApp:
            try prepareMarkdownFixture(workspace: workspace)
        case .filetreeApp:
            try prepareFileTreeFixture(workspace: workspace)
        case .charts, .todo, .markdown, .media, .surface, .canvas:
            break
        }
    }

    private func prepareUsageFixture(root: URL, workspace: URL) throws {
        try FileManager.default.createDirectory(
            at: root.appendingPathComponent("home", isDirectory: true),
            withIntermediateDirectories: true
        )
        try FileManager.default.createDirectory(
            at: root.appendingPathComponent("data", isDirectory: true),
            withIntermediateDirectories: true
        )
        let configDirectory = root.appendingPathComponent(
            "config/unpeel-usage",
            isDirectory: true
        )
        try FileManager.default.createDirectory(
            at: configDirectory,
            withIntermediateDirectories: true
        )
        try """
        refresh_secs = 3600
        theme = "dark"

        [claude]
        live_usage = false
        dirs = []

        [grok]
        live_usage = false
        """.write(
            to: configDirectory.appendingPathComponent("config.toml"),
            atomically: true,
            encoding: .utf8
        )
        try "Fixture project for the Usage master/detail screen.\n".write(
            to: workspace.appendingPathComponent("README.md"),
            atomically: true,
            encoding: .utf8
        )
    }

    private func prepareDiffsFixture(workspace: URL) throws {
        try initializeGitRepository(workspace)
        try "# Kitchen fixture\n\nOriginal line.\n".write(
            to: workspace.appendingPathComponent("README.md"),
            atomically: true,
            encoding: .utf8
        )
        try runFixtureCommand(["git", "add", "README.md"], in: workspace)
        try runFixtureCommand(["git", "commit", "-qm", "Initial fixture"], in: workspace)
        try "# Kitchen fixture\n\nEdited through the semantic Content detail.\n".write(
            to: workspace.appendingPathComponent("README.md"),
            atomically: true,
            encoding: .utf8
        )
        try "pub fn app_kit_fixture() -> bool { true }\n".write(
            to: workspace.appendingPathComponent("added.rs"),
            atomically: true,
            encoding: .utf8
        )
    }

    private func prepareIssuesFixture(root: URL, workspace: URL) throws {
        try initializeGitRepository(workspace)
        try "Kitchen Sink issue fixture.\n".write(
            to: workspace.appendingPathComponent("README.md"),
            atomically: true,
            encoding: .utf8
        )
        try runFixtureCommand(["git", "add", "README.md"], in: workspace)
        try runFixtureCommand(["git", "commit", "-qm", "Initial fixture"], in: workspace)
        try runFixtureCommand(["git", "checkout", "-qb", "feature/42-native-ui"], in: workspace)

        let bin = root.appendingPathComponent("bin", isDirectory: true)
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
        let gh = bin.appendingPathComponent("gh")
        try """
        #!/bin/sh
        set -eu
        case "$1:$2" in
          repo:view)
            printf '%s\n' '{"nameWithOwner":"unpeel/kitchen-fixture","url":"https://github.com/unpeel/kitchen-fixture"}'
            ;;
          issue:list)
            cat <<'JSON'
        [{"number":42,"title":"Render every issue screen natively","state":"OPEN","author":{"login":"tommy"},"labels":[{"name":"app-kit","color":"35c2b4"}],"updatedAt":"2026-09-01T08:30:00Z","url":"https://github.com/unpeel/kitchen-fixture/issues/42"},{"number":17,"title":"Keep terminal fallback exact","state":"OPEN","author":{"login":"agent"},"labels":[{"name":"terminal","color":"61afef"}],"updatedAt":"2026-08-31T12:00:00Z","url":"https://github.com/unpeel/kitchen-fixture/issues/17"}]
        JSON
            ;;
          issue:view)
            cat <<JSON
        {"number":$3,"title":"Render every issue screen natively","state":"OPEN","author":{"login":"tommy"},"assignees":[{"login":"agent"}],"labels":[{"name":"app-kit","color":"35c2b4"}],"body":"The complete issue body is carried by the read-only Content component.\\n\\n- Native SwiftUI\\n- Accessible web DOM\\n- Standalone Ratatui","createdAt":"2026-08-30T09:00:00Z","updatedAt":"2026-09-01T08:30:00Z","url":"https://github.com/unpeel/kitchen-fixture/issues/$3","comments":[{"author":{"login":"reviewer"},"body":"Verified through the Kitchen Sink mini-host.","createdAt":"2026-09-01T09:00:00Z","url":"https://github.com/unpeel/kitchen-fixture/issues/$3#issuecomment-1"}]}
        JSON
            ;;
          *)
            echo "unsupported fixture gh command: $*" >&2
            exit 2
            ;;
        esac
        """.write(to: gh, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: gh.path
        )
    }

    private func prepareMarkdownFixture(workspace: URL) throws {
        let vault = workspace.appendingPathComponent("vault", isDirectory: true)
        let nested = vault.appendingPathComponent("Projects", isDirectory: true)
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        try """
        # App Kit Markdown

        This document exercises source editing, native selection, task toggles, and menus.

        - [ ] Verify the Swift renderer
        - [x] Preserve the terminal renderer
        """.write(
            to: vault.appendingPathComponent("Welcome.md"),
            atomically: true,
            encoding: .utf8
        )
        try "# Nested note\n\nHierarchy stays semantic.\n".write(
            to: nested.appendingPathComponent("Roadmap.md"),
            atomically: true,
            encoding: .utf8
        )
    }

    private func prepareFileTreeFixture(workspace: URL) throws {
        let docs = workspace.appendingPathComponent("docs", isDirectory: true)
        try FileManager.default.createDirectory(at: docs, withIntermediateDirectories: true)
        try "# File Tree fixture\n".write(
            to: workspace.appendingPathComponent("README.md"),
            atomically: true,
            encoding: .utf8
        )
        try "Nested semantic Tree screen.\n".write(
            to: docs.appendingPathComponent("guide.txt"),
            atomically: true,
            encoding: .utf8
        )
    }
}

private func initializeGitRepository(_ workspace: URL) throws {
    try runFixtureCommand(["git", "init", "-q"], in: workspace)
    try runFixtureCommand(
        ["git", "config", "user.email", "kitchen@example.invalid"], in: workspace)
    try runFixtureCommand(["git", "config", "user.name", "Kitchen Sink"], in: workspace)
}

private func runFixtureCommand(_ arguments: [String], in directory: URL) throws {
    let process = Process()
    let diagnostics = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
    process.arguments = arguments
    process.currentDirectoryURL = directory
    process.standardOutput = diagnostics
    process.standardError = diagnostics
    try process.run()
    let output = diagnostics.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
        let message = String(data: output, encoding: .utf8) ?? "fixture command failed"
        throw FixtureError.command(arguments.joined(separator: " "), message)
    }
}

private enum FixtureError: LocalizedError {
    case command(String, String)

    var errorDescription: String? {
        switch self {
        case .command(let command, let output):
            "Kitchen fixture command failed (\(command)): \(output)"
        }
    }
}
