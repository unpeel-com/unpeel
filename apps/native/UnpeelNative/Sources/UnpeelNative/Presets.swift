//
//  Presets.swift
//  UnpeelNative
//
//  Quick-launch preset model + selection rules. The preset list is FLAT and
//  user-ordered (no per-CLI sections): the CLI is auto-detected from the
//  command head, the list order is a native UserDefaults overlay, and the
//  topmost enabled preset of a CLI doubles as that CLI's default for MCP
//  session launches. Originally ported from the Svelte app:
//  - Preset shape: stores/presets.ts:5-12
//  - Tool detection from a command head: tools/registry.ts:79-88
//  - Availability gating: stores/aiTools.ts:30-34 (unknown report = all
//    available) backed by the same PATH search as setup.rs:108-123.
//
//  The native app is GLOBAL-presets-only (deliberate simplification away
//  from the Tauri app's <project>/.unpeel.json project presets).
//
//  Data source (READ ONLY):
//  - global presets: ~/.unpeel/app-state.json `presets`
//

import Foundation
import UnpeelShared

// MARK: - Model

struct Preset: Identifiable, Hashable, Codable {
    var id: String
    var label: String
    var command: String
    var enabled: Bool
    var quickLaunch: Bool

    /// Snake_case keys to match the Rust Preset shape, so the native
    /// UserDefaults overlay stores the same JSON dialect as app-state.json.
    enum CodingKeys: String, CodingKey {
        case id, label, command
        case enabled
        case quickLaunch = "quick_launch"
    }
}

/// One installed Unpeel App the launch list can offer to add as a preset.
/// Sourced from `unpeel-host __apps__ list`; Rust resolves the central App
/// catalog against the Host's PATH so native does not duplicate discovery.
struct InstalledAppInfo: Identifiable, Hashable, Codable, Sendable {
    let id: String
    let name: String
    var version: String? = nil
    let command: String
    var description: String = ""
    var tint: String? = nil
}

extension Preset {
    /// Blank-terminal pseudo-preset (stores/presets.ts:14-23).
    static let newTerminalID = "__new_terminal__"
    static let newTerminal = Preset(
        id: newTerminalID,
        label: "Terminal",
        command: "",
        enabled: true,
        quickLaunch: false
    )

    var isNewTerminal: Bool { id == Preset.newTerminalID }

    /// Generated from `runtimes/*/runtime.toml`; used as setup first-run
    /// fallback content when app-state has not been created yet.
    static let builtinGlobalPresets: [Preset] = UnpeelRuntimeCatalog
        .runtimes(for: .macos)
        .flatMap { runtime in
        runtime.suggestedPresets.map { preset in
            Preset(
                id: preset.id,
                label: preset.label,
                command: preset.command,
                enabled: true,
                quickLaunch: preset.quickLaunch
            )
        }
    }

    static let recommendedFavoritePresets: [Preset] = builtinGlobalPresets.filter(\.quickLaunch)

    /// Tool the command maps to (nil for plain shell commands).
    var tool: QuickPresetTool? { QuickPresetTool.detect(in: command) }

    /// sanitize_preset_quick_launch (state.rs:228): quick_launch is only
    /// honored for supported tool commands.
    func sanitized() -> Preset {
        var copy = self
        // Any known CLI can be quick-launched (favorited), not just the six
        // QuickPresetTool tools — so gate on SetupTool, not `tool`.
        copy.quickLaunch = quickLaunch && SetupTool.detect(in: command) != nil
        return copy
    }
}

/// Catalog-backed identity used by quick-preset presentation. This used to be
/// a closed six-case enum; keeping it open means a new `runtime.toml` becomes
/// favorite-capable without adding a Swift case.
struct QuickPresetTool: RawRepresentable, CaseIterable, Hashable, Identifiable, Sendable {
    let rawValue: String

    init?(rawValue: String) {
        guard let runtime = UnpeelRuntimeCatalog.runtime(id: rawValue, for: .macos),
              runtime.supportsQuickLaunch
        else { return nil }
        self.rawValue = runtime.legacySlug
    }

    private init(runtime: UnpeelRuntimeMetadata) {
        rawValue = runtime.legacySlug
    }

    private init(uncheckedRawValue: String) {
        rawValue = uncheckedRawValue
    }

    static var allCases: [QuickPresetTool] {
        UnpeelRuntimeCatalog.runtimes(for: .macos)
            .filter(\.supportsQuickLaunch)
            .map(QuickPresetTool.init(runtime:))
    }

    /// Descriptor order is the compatibility and first-run tie-break order.
    static var order: [QuickPresetTool] { allCases }

    var id: String { rawValue }
    var metadata: UnpeelRuntimeMetadata? { UnpeelRuntimeCatalog.runtime(id: rawValue) }
    var displayName: String { metadata?.label ?? rawValue.capitalized }
    var iconKey: String { metadata?.iconKey ?? "agent" }

    static func detect(in command: String) -> QuickPresetTool? {
        guard let runtime = UnpeelRuntimeCatalog.runtime(command: command, for: .macos),
              runtime.supportsQuickLaunch
        else { return nil }
        return QuickPresetTool(runtime: runtime)
    }

    // Source-compatibility conveniences for existing icon and test code.
    static let claude = QuickPresetTool(uncheckedRawValue: "claude")
    static let codex = QuickPresetTool(uncheckedRawValue: "codex")
    static let amp = QuickPresetTool(uncheckedRawValue: "amp")
    static let gemini = QuickPresetTool(uncheckedRawValue: "gemini")
    static let pi = QuickPresetTool(uncheckedRawValue: "pi")
    static let opencode = QuickPresetTool(uncheckedRawValue: "opencode")
}

// MARK: - Setup tool detection

/// CLIs Unpeel knows how to detect during setup. This intentionally includes
/// tools that are not quick-launchable so the Presets panel can show the full local
/// agent surface while the favorites preview stays scoped to quick tools.
struct SetupTool: RawRepresentable, CaseIterable, Identifiable, Hashable, Sendable {
    let rawValue: String

    init?(rawValue: String) {
        guard let runtime = UnpeelRuntimeCatalog.runtime(id: rawValue, for: .macos) else {
            return nil
        }
        self.rawValue = runtime.legacySlug
    }

    private init(runtime: UnpeelRuntimeMetadata) {
        rawValue = runtime.legacySlug
    }

    private init(uncheckedRawValue: String) {
        rawValue = uncheckedRawValue
    }

    static var allCases: [SetupTool] {
        UnpeelRuntimeCatalog.runtimes(for: .macos).map(SetupTool.init(runtime:))
    }

    var id: String { rawValue }
    var metadata: UnpeelRuntimeMetadata? { UnpeelRuntimeCatalog.runtime(id: rawValue) }
    var displayName: String { metadata?.label ?? rawValue.capitalized }
    var commandNames: [String] { metadata?.commandAliases ?? [rawValue] }
    var commandName: String { metadata?.commandAliases.first ?? rawValue }

    var defaultPresetCommand: String {
        metadata?.defaultPreset?.command ?? commandName
    }

    var quickPresetTool: QuickPresetTool? {
        QuickPresetTool(rawValue: rawValue)
    }

    /// Every known CLI can be favorited (quick-launched) now.
    var isFavoriteCapable: Bool {
        metadata?.supportsQuickLaunch ?? false
    }

    /// Non-interactive shell one-liner that installs this CLI, run in the
    /// user's login shell by the Presets panel's Install button. Only vendors whose
    /// official install command is known get one — a guessed package name
    /// could install a squatted lookalike. The rest link to `websiteURL`.
    var installCommand: String? {
        metadata?.installCommand
    }

    /// Vendor page with install instructions — the fallback when there is no
    /// trusted `installCommand`, or when an install attempt fails.
    var websiteURL: URL? {
        metadata?.installURL.flatMap(URL.init(string:))
    }

    /// Tools that report lifecycle through provider hooks. For these, raw
    /// terminal output is not a reliable busy signal: full-screen TUIs repaint
    /// while the user scrolls, which can otherwise make idle sessions spin.
    var usesLifecycleHooks: Bool {
        metadata?.capabilities.contains(.lifecycleHooks) == true
    }

    /// Resolve a command to the CLI it launches by its first whitespace-
    /// separated word (covers all known CLIs, unlike `QuickPresetTool.detect`
    /// which only knows the six quick tools). Unknown heads (custom commands)
    /// return nil and are treated as always-available.
    static func detect(in command: String) -> SetupTool? {
        guard let runtime = UnpeelRuntimeCatalog.runtime(command: command, for: .macos) else {
            return nil
        }
        return SetupTool(runtime: runtime)
    }

    // Source-compatibility conveniences. Runtime discovery is catalog-backed;
    // these names are only for existing provider-specific presentation code.
    static let claude = SetupTool(uncheckedRawValue: "claude")
    static let codex = SetupTool(uncheckedRawValue: "codex")
    static let cline = SetupTool(uncheckedRawValue: "cline")
    static let cursorAgent = SetupTool(uncheckedRawValue: "cursor-agent")
    static let grok = SetupTool(uncheckedRawValue: "grok")
    static let kimi = SetupTool(uncheckedRawValue: "kimi")
    static let kiro = SetupTool(uncheckedRawValue: "kiro-cli")
    static let muse = SetupTool(uncheckedRawValue: "muse")
    static let opencode = SetupTool(uncheckedRawValue: "opencode")
    static let amp = SetupTool(uncheckedRawValue: "amp")
    static let gemini = SetupTool(uncheckedRawValue: "gemini")
    static let pi = SetupTool(uncheckedRawValue: "pi")
    static let copilot = SetupTool(uncheckedRawValue: "copilot")
}

struct ToolInstallStatus: Identifiable, Equatable, Sendable {
    let tool: SetupTool
    let path: String?
    var usage: ToolUsageStats = .none

    var id: String { tool.id }
    var installed: Bool { path != nil }
}

/// How much a CLI has actually been used on this machine, measured from the
/// provider's own on-disk conversation store (the same dirs the transcript
/// API reads) — session file count plus recency. Lets first-run seeding rank
/// an existing user's CLIs by real usage instead of a fixed order.
struct ToolUsageStats: Equatable, Sendable {
    /// Total session files found in the provider's store.
    let sessionCount: Int
    /// Session files modified within the recent-usage window (30 days).
    let recentCount: Int
    /// Most recent session file modification.
    let lastUsed: Date?

    static let none = ToolUsageStats(sessionCount: 0, recentCount: 0, lastUsed: nil)

    var hasAny: Bool { sessionCount > 0 }

    /// Ordering: recent activity beats lifetime volume, volume beats recency
    /// of a single stray file.
    static func moreUsed(_ a: ToolUsageStats, _ b: ToolUsageStats) -> Bool {
        if a.recentCount != b.recentCount { return a.recentCount > b.recentCount }
        if a.sessionCount != b.sessionCount { return a.sessionCount > b.sessionCount }
        return (a.lastUsed ?? .distantPast) > (b.lastUsed ?? .distantPast)
    }

    /// Human usage summary, e.g. "342 sessions · used today".
    var summary: String? {
        guard sessionCount > 0 else { return nil }
        let sessions = sessionCount == 1 ? "1 session" : "\(sessionCount) sessions"
        guard let lastUsed else { return sessions }
        let calendar = Calendar.current
        if calendar.isDateInToday(lastUsed) { return "\(sessions) · used today" }
        if calendar.isDateInYesterday(lastUsed) { return "\(sessions) · used yesterday" }
        let days = calendar.dateComponents(
            [.day], from: calendar.startOfDay(for: lastUsed), to: calendar.startOfDay(for: Date())
        ).day ?? 0
        if days <= 60 { return "\(sessions) · used \(days) days ago" }
        return sessions
    }
}

struct ToolScanReport: Equatable, Sendable {
    let statuses: [ToolInstallStatus]

    var installedStatuses: [ToolInstallStatus] {
        statuses.filter(\.installed)
    }

    var missingStatuses: [ToolInstallStatus] {
        statuses.filter { !$0.installed }
    }

    var anyAIInstalled: Bool {
        installedStatuses.isEmpty == false
    }

    var installedQuickTools: Set<QuickPresetTool> {
        Set(installedStatuses.compactMap { $0.tool.quickPresetTool })
    }

    func status(for tool: SetupTool) -> ToolInstallStatus? {
        statuses.first { $0.tool == tool }
    }

    /// The installed CLI with the clearest usage lead, for a
    /// "Most used" badge. Requires a handful of sessions so one stray file
    /// on a fresh machine doesn't get badged.
    var mostUsedTool: SetupTool? {
        let ranked = installedStatuses
            .filter { $0.usage.sessionCount >= 3 }
            .sorted { ToolUsageStats.moreUsed($0.usage, $1.usage) }
        return ranked.first?.tool
    }

    /// Installed CLIs ordered most-used first (unused ones keep their
    /// declaration order at the end). Used to seed the CLI order on first run.
    var usageOrderedInstalledTools: [SetupTool] {
        installedStatuses
            .enumerated()
            .sorted { a, b in
                if a.element.usage != b.element.usage {
                    return ToolUsageStats.moreUsed(a.element.usage, b.element.usage)
                }
                // Equal stats (usually both zero): keep declaration order.
                return a.offset < b.offset
            }
            .map { $0.element.tool }
    }
}

// MARK: - Selection (quick strip grouping)

/// Starred presets of one CLI, in flat-list order. The sidebar quick strip
/// renders one chip per group: a single starred preset launches directly,
/// two or more turn the chip into a dropdown menu.
struct QuickPresetGroup: Identifiable, Equatable {
    let cli: SetupTool
    let presets: [Preset]

    var id: String { cli.rawValue }
    /// The group's launch target for single-click surfaces (⌘N, snapshot
    /// tests): the topmost starred preset — consistent with the
    /// order-derived per-CLI default.
    var leader: Preset { presets[0] }
}

/// Group starred, enabled presets by CLI, preserving the flat list order
/// (groups ordered by their first starred preset; presets within a group
/// keep list order). Unknown-head commands can't be starred (`sanitized()`),
/// so every group has a CLI.
func collectQuickPresetGroups(_ items: [Preset]) -> [QuickPresetGroup] {
    var cliOrder: [SetupTool] = []
    var byCLI: [SetupTool: [Preset]] = [:]

    for preset in items {
        guard preset.quickLaunch, preset.enabled else { continue }
        guard let cli = SetupTool.detect(in: preset.command) else { continue }
        if byCLI[cli] == nil { cliOrder.append(cli) }
        byCLI[cli, default: []].append(preset)
    }

    return cliOrder.map { QuickPresetGroup(cli: $0, presets: byCLI[$0] ?? []) }
}

// MARK: - On-disk decoding

/// Entry of the global `presets` array in app-state.json (state.rs Preset).
/// `project_id` is decoded only to FILTER OUT legacy project-scoped entries
/// that predate the Tauri app's migration to <project>/.unpeel.json — the
/// native app is global-presets-only.
struct GlobalPresetFile: Decodable {
    let id: String
    let label: String
    let command: String
    let projectID: String?
    let enabled: Bool?
    let quickLaunch: Bool?

    enum CodingKeys: String, CodingKey {
        case id, label, command
        case projectID = "project_id"
        case enabled
        case quickLaunch = "quick_launch"
    }

    var runtime: Preset {
        Preset(
            id: id,
            label: label,
            command: command,
            enabled: enabled ?? true,
            // sanitize_preset_quick_launch (state.rs:228): quick_launch is
            // only honored for supported tool commands.
            quickLaunch: (quickLaunch ?? false) && SetupTool.detect(in: command) != nil
        )
    }
}

// MARK: - Tool usage (provider session stores)

/// Counts each provider's on-disk session files to estimate real usage.
/// Roots mirror the transcript API's provider adapters (transcripts.rs):
/// these are the CLIs' own conversation stores, so they reflect usage that
/// predates Unpeel — exactly what first-run seeding wants to rank by.
enum ToolUsageScanner {
    /// Recent-usage window for `recentCount`.
    static let recentWindow: TimeInterval = 30 * 24 * 60 * 60
    /// Stop enumerating after this many matches; ordering is long settled by
    /// then and first-run must stay snappy on huge stores.
    private static let sessionCountCap = 5000

    struct Store {
        let root: String            // relative to home
        let extensions: Set<String>
        /// When set, only count this exact session filename.
        var fileName: String? = nil
        /// When set, only count session files with this suffix.
        var fileNameSuffix: String? = nil
        /// When set, only count files directly inside a dir with this name
        /// (e.g. gemini keeps chats among other tmp noise).
        var parentDirName: String? = nil
    }

    static func stores(for tool: SetupTool) -> [Store] {
        tool.metadata?.usageStores.map { store in
            Store(
                root: store.root,
                extensions: store.extensions,
                fileName: store.fileName,
                fileNameSuffix: store.fileNameSuffix,
                parentDirName: store.parentDirName
            )
        } ?? []
    }

    static func stats(for tool: SetupTool, now: Date = Date()) -> ToolUsageStats {
        let stores = stores(for: tool)
        guard !stores.isEmpty else { return .none }

        var count = 0
        var recent = 0
        var lastUsed: Date?
        let recentCutoff = now.addingTimeInterval(-recentWindow)

        for store in stores {
            let root = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(store.root)
            guard let enumerator = FileManager.default.enumerator(
                at: root,
                includingPropertiesForKeys: [.isRegularFileKey, .contentModificationDateKey],
                options: [.skipsHiddenFiles, .skipsPackageDescendants]
            ) else { continue }

            for case let url as URL in enumerator {
                guard store.extensions.contains(url.pathExtension.lowercased()) else { continue }
                if let fileName = store.fileName, url.lastPathComponent != fileName {
                    continue
                }
                if let suffix = store.fileNameSuffix, !url.lastPathComponent.hasSuffix(suffix) {
                    continue
                }
                if let parent = store.parentDirName,
                   url.deletingLastPathComponent().lastPathComponent != parent {
                    continue
                }
                guard let values = try? url.resourceValues(
                    forKeys: [.isRegularFileKey, .contentModificationDateKey]
                ), values.isRegularFile == true else { continue }

                count += 1
                if let modified = values.contentModificationDate {
                    if modified > recentCutoff { recent += 1 }
                    if lastUsed.map({ modified > $0 }) ?? true { lastUsed = modified }
                }
                if count >= sessionCountCap { break }
            }
            if count >= sessionCountCap { break }
        }
        return ToolUsageStats(sessionCount: count, recentCount: recent, lastUsed: lastUsed)
    }
}

// MARK: - Tool availability (setup.rs search_dirs + aiTools.ts)

/// Resolves which AI tool binaries exist, using the same search strategy as
/// setup.rs: process PATH + interactive-shell PATH + common bin dirs.
/// Until the (async, shell-spawning) scan completes, `installed` is nil and
/// every preset counts as available — matching isPresetAvailable's
/// null-report behavior in aiTools.ts:30-34.
final class ToolAvailability: @unchecked Sendable {
    private let lock = NSLock()
    private var _installed: Set<QuickPresetTool>?
    private var _report: ToolScanReport?

    var installed: Set<QuickPresetTool>? {
        lock.lock()
        defer { lock.unlock() }
        return _installed
    }

    var report: ToolScanReport? {
        lock.lock()
        defer { lock.unlock() }
        return _report
    }

    func isAvailable(command: String) -> Bool {
        guard let cli = SetupTool.detect(in: command) else { return true }
        guard let report else { return true }
        return report.status(for: cli)?.installed ?? true
    }

    /// Kick off the background scan. The optional completion runs on the
    /// scanner queue after the internal availability cache is updated; callers
    /// that touch UI state should hop to the main actor.
    func scan(completion: (@Sendable (ToolScanReport) -> Void)? = nil) {
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let dirs = Self.searchDirs()
            let statuses = SetupTool.allCases.map { tool in
                ToolInstallStatus(
                    tool: tool,
                    path: Self.findCommandPath(tool.commandNames, dirs: dirs),
                    usage: ToolUsageScanner.stats(for: tool)
                )
            }
            let report = ToolScanReport(statuses: statuses)
            var found: Set<QuickPresetTool> = []
            for status in statuses where status.installed {
                if let quickTool = status.tool.quickPresetTool {
                    found.insert(quickTool)
                }
            }
            guard let self else { return }
            self.lock.lock()
            self._installed = found
            self._report = report
            self.lock.unlock()
            completion?(report)
        }
    }

    private static func findCommandPath(_ name: String, dirs: [URL]) -> String? {
        for dir in dirs {
            let candidate = dir.appendingPathComponent(name).path
            if FileManager.default.isExecutableFile(atPath: candidate) {
                return candidate
            }
        }
        return nil
    }

    private static func findCommandPath(_ names: [String], dirs: [URL]) -> String? {
        names.lazy.compactMap { findCommandPath($0, dirs: dirs) }.first
    }

    private static func searchDirs() -> [URL] {
        var seen = Set<String>()
        var dirs: [URL] = []

        func append(_ path: String) {
            guard !path.isEmpty, seen.insert(path).inserted else { return }
            dirs.append(URL(fileURLWithPath: path))
        }

        // Process PATH.
        for entry in (ProcessInfo.processInfo.environment["PATH"] ?? "").split(separator: ":") {
            append(String(entry))
        }
        // Interactive shell PATH ($SHELL -i -c, setup.rs:49-67).
        for entry in shellPath().split(separator: ":") {
            append(String(entry))
        }
        // Common bin dirs (setup.rs:69-90).
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        for path in [
            "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin",
            "\(home)/.bun/bin",
            "\(home)/.cargo/bin", "\(home)/.local/bin",
            "\(home)/.npm-global/bin", "\(home)/Library/pnpm",
            "\(home)/.local/share/pnpm", "\(home)/bin",
        ] {
            append(path)
        }
        for suffix in UnpeelRuntimeCatalog.runtimes(for: .macos).flatMap(\.searchPathSuffixes) {
            append(URL(fileURLWithPath: home).appendingPathComponent(suffix).path)
        }

        return dirs
    }

    private static func shellPath() -> String {
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        let start = "__UNPEEL_PATH_START__"
        let end = "__UNPEEL_PATH_END__"

        let process = Process()
        process.executableURL = URL(fileURLWithPath: shell)
        process.arguments = ["-i", "-c", "printf '\(start)%s\(end)' \"$PATH\""]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        process.standardInput = FileHandle.nullDevice

        guard (try? process.run()) != nil else { return "" }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()

        let combined = String(data: data, encoding: .utf8) ?? ""
        guard let startRange = combined.range(of: start),
              let endRange = combined.range(of: end, range: startRange.upperBound..<combined.endIndex)
        else { return "" }
        return String(combined[startRange.upperBound..<endRange.lowerBound])
    }
}
