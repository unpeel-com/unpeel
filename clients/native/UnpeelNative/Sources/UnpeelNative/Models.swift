//
//  Models.swift
//  UnpeelNative
//
//  Plain data types decoded from the existing Unpeel on-disk contracts
//  (~/.unpeel/app-state.json and app-sessions/*/manifest.json). These are
//  READ paths only; the shapes must track the Rust structs in
//  apps/desktop/src-tauri/src/state.rs and session_host.rs.
//

import Foundation
import UnpeelShared

enum CommandTAction: String, CaseIterable, Identifiable {
    case newTerminal
    case presetPicker

    var id: String { rawValue }

    var title: String {
        switch self {
        case .newTerminal: return "New terminal"
        case .presetPicker: return "Preset screen"
        }
    }
}

enum SessionTitleDefaults {
    /// Agent sessions begin with their command. A blank shell begins with its
    /// working folder and is replaced by the Host after the first submitted
    /// command, just like the former provisional "Terminal" label.
    static func initialLabel(command: String, cwd: String) -> String {
        guard command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return command
        }
        let folder = abbreviatedPath(cwd)
        return folder.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? "Terminal"
            : folder
    }

    static func abbreviatedPath(_ path: String, home: String = NSHomeDirectory()) -> String {
        if path == home { return "~" }
        if path.hasPrefix(home + "/") {
            return "~" + path.dropFirst(home.count)
        }
        return path
    }
}

// MARK: - app-state.json

struct Project: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let path: String
    /// Plain groups may be pinned above the parent's ordinary mixed rows.
    /// Optional/additive so older app-state files remain unchanged.
    var pinnedAt: UInt64? = nil
    let parentProjectID: String?
    let sortOrder: Int?
    let isFolder: Bool?
    let worktreeBranch: String?
    let workspacesEnabled: Bool?
    let mcpBlocked: Bool?

    enum CodingKeys: String, CodingKey {
        case id, name, path
        case pinnedAt = "pinned_at"
        case parentProjectID = "parent_project_id"
        case sortOrder = "sort_order"
        case isFolder = "is_folder"
        case worktreeBranch = "worktree_branch"
        case workspacesEnabled = "workspaces_enabled"
        case mcpBlocked = "mcp_blocked"
    }

    var isWorktree: Bool { worktreeBranch != nil && parentProjectID != nil }

    /// Only plain organizational child groups accept a running session drop.
    /// Worktree children need an explicit restart/resume to change checkout,
    /// while top-level projects remain reorder targets rather than filing
    /// targets.
    var acceptsSessionDrop: Bool {
        parentProjectID != nil && worktreeBranch == nil && isFolder == true
    }
}

/// Pinned sidebar session, mirroring `PinnedSidebarSession` in state.rs:177.
/// `key` is `"session:<session-id>"`; pins sort newest-first by `pinned_at`.
/// Additive since 2026-08: a pinned child GROUP also gets a record with
/// `key = "project:<group-id>"` and no `session_id`, so the per-project
/// records array is the ONE mixed pinned order (sessions and groups
/// interleaved). Old readers `compactMap(\.sessionID)` skip these safely,
/// and the group's own `pinned_at` project marker remains the
/// cross-frontend pin truth — the record is only an ordering entry.
struct PinnedSidebarSession: Codable, Hashable, Identifiable, Sendable {
    let key: String
    let projectID: String
    let sessionID: String?
    let pinnedAt: UInt64

    var id: String { key }

    enum CodingKeys: String, CodingKey {
        case key
        case projectID = "project_id"
        case sessionID = "session_id"
        case pinnedAt = "pinned_at"
    }

    static func key(forSessionID sessionID: String) -> String {
        "session:\(sessionID)"
    }

    static func key(forProjectID projectID: String) -> String {
        "project:\(projectID)"
    }

    /// The pinned child group's project id for a `"project:"` record; nil
    /// for ordinary session pins.
    var pinnedProjectID: String? {
        guard sessionID == nil, key.hasPrefix("project:") else { return nil }
        let id = String(key.dropFirst("project:".count))
        return id.isEmpty ? nil : id
    }

    /// The sidebar row this record ranks: the session id, or the pinned
    /// child group's project id.
    var orderTargetID: String? { sessionID ?? pinnedProjectID }
}

private enum PinnedSessionsFile: Decodable {
    case byProject([String: [PinnedSidebarSession]])
    case legacy([PinnedSidebarSession])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let byProject = try? container.decode([String: [PinnedSidebarSession]].self) {
            self = .byProject(byProject)
            return
        }
        if let legacy = try? container.decode([PinnedSidebarSession].self) {
            self = .legacy(legacy)
            return
        }
        throw DecodingError.typeMismatch(
            PinnedSessionsFile.self,
            DecodingError.Context(
                codingPath: decoder.codingPath,
                debugDescription: "Expected pinned_sessions map or legacy array"
            )
        )
    }

    var byProject: [String: [PinnedSidebarSession]] {
        switch self {
        case let .byProject(pins):
            return pins
        case let .legacy(pins):
            return Dictionary(grouping: pins, by: \.projectID)
        }
    }
}

/// Legacy reach half of an `McpGrant`. The current UI exposes roles only and
/// writes project reach, but this stays readable for old app-state files.
enum OrchestratorReach: String, CaseIterable, Identifiable {
    case project
    case global

    var id: String { rawValue }

    static func fromStateString(_ value: String?) -> OrchestratorReach? {
        switch value?.trimmingCharacters(in: .whitespaces).lowercased() {
        case "project": return .project
        case "global": return .global
        default: return nil
        }
    }

    /// Short legacy label.
    var label: String {
        switch self {
        case .project: return "Project"
        case .global: return "Global"
        }
    }

    /// One-line legacy description.
    var reachDescription: String {
        switch self {
        case .project: return "Manages sessions in this project"
        case .global: return "Manages all sessions"
        }
    }
}

/// The capability half of a session's Unpeel Sessions MCP grant. Mirrors
/// `McpRole` in `crates/.../state.rs`.
enum McpRole: String, CaseIterable, Identifiable {
    case read
    case write

    var id: String { rawValue }

    static func fromStateString(_ value: String?) -> McpRole {
        switch value?.trimmingCharacters(in: .whitespaces).lowercased() {
        case "write", "writer", "orchestrator": return .write
        default: return .read
        }
    }
}

/// A per-session access grant: a role (capability) plus a reach. Persisted in
/// the `mcp_orchestrators` map keyed by session id as `{ role, reach }`.
/// Sessions absent from the map use `McpGrant.default` (Read at project reach).
/// Mirrors `McpGrant` in the Rust core.
struct McpGrant: Equatable {
    var role: McpRole
    var reach: OrchestratorReach

    /// The default grant for a session with no explicit override.
    static let `default` = McpGrant(role: .read, reach: .project)

    /// The two user-facing access levels collapse role+reach into one choice.
    var accessLevel: SessionAccessLevel {
        switch role {
        case .read: return .read
        case .write: return .readWrite
        }
    }
}

/// App-wide policy for Sessions MCP writes to any other session. The
/// persisted key remains `mcp_nonchild_write_access` for compatibility;
/// sidebar groups are organizational and never bypass the policy.
enum McpNonChildWriteAccess: String, CaseIterable, Identifiable {
    /// Ask the user to approve the caller→target pair the first time; the
    /// approval is remembered in `mcp_write_approvals` until either session
    /// is removed. The shipped default.
    case ask
    /// Never allow writes to another session.
    case deny
    /// Allow any session to write to any other session without asking.
    case allow

    var id: String { rawValue }

    /// Lenient parse matching the Rust `from_state_str`: unknown/missing
    /// values fall back to `.ask` (the shipped default).
    static func fromStateString(_ value: String?) -> McpNonChildWriteAccess {
        switch value?.trimmingCharacters(in: .whitespaces).lowercased() {
        case "deny": return .deny
        case "allow": return .allow
        default: return .ask
        }
    }

    /// Picker label.
    var label: String {
        switch self {
        case .ask: return "Ask for approval"
        case .deny: return "Never allow"
        case .allow: return "Always allow"
        }
    }

    /// One-line description shown under the label in Settings.
    var detail: String {
        switch self {
        case .ask:
            return "Show an approval dialog the first time a session writes to one "
                + "of your other sessions. Approvals are remembered per pair."
        case .deny:
            return "Sessions cannot write to other sessions."
        case .allow:
            return "Any session can write to any other session without asking."
        }
    }
}

/// Legacy two-level projection retained while older `McpGrant` state remains
/// readable. Current Sessions use is open for reads, uses the separate write
/// approval policy, and leaves session lifecycle with the user.
enum SessionAccessLevel: String, CaseIterable, Identifiable {
    case read
    case readWrite

    var id: String { rawValue }

    var grant: McpGrant {
        switch self {
        case .read: return McpGrant(role: .read, reach: .project)
        case .readWrite: return McpGrant(role: .write, reach: .project)
        }
    }

    /// Menu/picker label.
    var label: String {
        switch self {
        case .read: return "Read"
        case .readWrite: return "Legacy write access"
        }
    }

    /// One-line description shown under the label in Settings.
    var detail: String {
        switch self {
        case .read:
            return "Legacy saved grant. Session reads are now open across the workspace."
        case .readWrite:
            return "Legacy saved grant. Writes now use the separate approval policy; agents cannot create or close sessions."
        }
    }

    /// Whether this level grants the elevated write capability.
    var canWriteSessions: Bool {
        switch self {
        case .readWrite: return true
        case .read: return false
        }
    }
}

/// Read-tolerant decoder for one `mcp_orchestrators` value. Accepts the new
/// object form `{ "role": ..., "reach": ... }` and the legacy bare-string form
/// (`"project"`/`"global"`), which decodes to the write role at project reach
/// — matching the Rust `McpGrant` deserializer.
private struct McpGrantFile: Decodable {
    let grant: McpGrant

    private enum CodingKeys: String, CodingKey {
        case role
        case reach
    }

    init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer(),
           let legacy = try? single.decode(String.self) {
            let _ = legacy
            grant = McpGrant(role: .write, reach: .project)
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let roleString = (try? container.decodeIfPresent(String.self, forKey: .role)) ?? nil
        grant = McpGrant(
            role: McpRole.fromStateString(roleString),
            reach: .project
        )
    }
}

/// Host-owned semantic App/panel associations. Controllers read this additive
/// envelope but never write pane geometry back into it.
struct AppPresentationsFile: Decodable {
    struct Instance: Decodable {
        let id: String
        let appID: String
        let companionSessionID: String

        enum CodingKeys: String, CodingKey {
            case id
            case appID = "app_id"
            case companionSessionID = "companion_session_id"
        }
    }

    struct Presentation: Decodable {
        let id: String
        let callerSessionID: String
        let instanceID: String
        let target: String
        let revealRevision: UInt64

        enum CodingKeys: String, CodingKey {
            case id
            case callerSessionID = "caller_session_id"
            case instanceID = "instance_id"
            case target
            case revealRevision = "reveal_revision"
        }
    }

    let version: Int
    let instances: [Instance]
    let presentations: [Presentation]

    enum CodingKeys: String, CodingKey {
        case version, instances, presentations
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = (try? container.decode(Int.self, forKey: .version)) ?? 0
        instances = (try? container.decode([Instance].self, forKey: .instances)) ?? []
        presentations = (try? container.decode(
            [Presentation].self, forKey: .presentations
        )) ?? []
    }
}

/// What drives a session's automatic title — `session_title_mode` in
/// app-state.json (state.rs `SessionTitleMode`). A user rename permanently
/// wins in every mode; the session hosts read the knob live, so a change
/// applies without restarting sessions.
enum SessionTitleMode: String, CaseIterable, Identifiable {
    case firstPrompt = "first_prompt"
    case agent
    case off

    var id: String { rawValue }

    var title: String {
        switch self {
        case .firstPrompt: return "First prompt"
        case .agent: return "Live from agent"
        case .off: return "Off"
        }
    }
}

struct AppStateFile: Decodable {
    let projects: [Project]
    /// `pinned_sessions` map (project id -> pins), with the legacy flat-array
    /// shape grouped by each pin's project id to match the Rust loader.
    let pinnedSessions: [String: [PinnedSidebarSession]]
    /// Global presets (`presets` array, state.rs Preset). Read-tolerant.
    /// Legacy project-scoped entries (project_id != nil) are dropped — the
    /// native app is global-presets-only.
    let presets: [Preset]
    /// `code_editor` (state.rs:211-212, default "code") — the editor id the
    /// "Open in <Editor>" project-menu item launches.
    let codeEditor: String
    /// `theme` (state.rs, default "system") — the Tauri Appearance tab's
    /// mode. nil when absent/unrecognized; the native overlay wins over it.
    let theme: ThemePreference?
    /// `mcp_orchestrators` (state.rs) — per-session access overrides the host
    /// enforces, keyed by session id. Absent ids use `McpGrant.default`.
    let mcpOrchestrators: [String: McpGrant]
    /// `mcp_blocked_projects` (state.rs) — project ids explicitly blocked
    /// from MCP, keyed by id so overlay-only/worktree projects can be blocked
    /// even though they never appear in `projects`.
    let mcpBlockedProjects: [String]
    /// `setup_completed` (state.rs) — legacy flag from the removed first-run
    /// setup wizard; still read so migrations stay one-shot for users who
    /// completed it.
    let setupCompleted: Bool
    /// `mcp_default_access` (state.rs) — the app-wide default access grant for
    /// sessions without an explicit override. Absent ⇒ `McpGrant.default`.
    let mcpDefaultAccess: McpGrant
    /// `browser_default_access` (state.rs) — the app-wide Browser Access. Absent
    /// ⇒ on (the shipped default; Settings ▸ Browser ▸ Off is the master
    /// disable). Browser access is a single global on/off — no per-session map.
    let browserDefaultAccess: BrowserAccess
    /// `browser_settings` (state.rs) — app-wide engine options the
    /// `__browser_mcp__` server reads per call. Absent ⇒ defaults.
    let browserSettings: BrowserSettings
    /// `transcript_settings` (state.rs) — app-wide transcript rendering options
    /// the host reads when building the Markdown for "Copy Transcript" and the
    /// Sessions MCP `read_transcript` tool. Absent ⇒ defaults.
    let transcriptSettings: TranscriptSettings
    /// `mcp_nonchild_write_access` (state.rs) — the app-wide policy for
    /// Sessions MCP writes to another session. Absent ⇒ `.ask`.
    let mcpNonChildWriteAccess: McpNonChildWriteAccess
    /// `mcp_write_approvals` (state.rs) — user-approved write pairs,
    /// caller session id → approved target session ids. Absent ⇒ empty.
    let mcpWriteApprovals: [String: [String]]
    /// `mcp_app_open_approvals` — user-approved App launch pairs, caller
    /// session id → installed App ids. Absent ⇒ empty.
    let mcpAppOpenApprovals: [String: [String]]
    /// `app_presentations` — semantic Host bindings projected by each
    /// Controller into its own local pane/detail layout.
    let appPresentations: AppPresentationsFile?
    /// `computer_default_access` (state.rs) — the app-wide Computer MCP mode.
    /// Absent ⇒ ask (the shipped default); explicit junk ⇒ off.
    let computerDefaultAccess: ComputerAccess
    /// `computer_approvals` (state.rs) — session ids the user approved for
    /// computer use under Ask. Absent ⇒ empty.
    let computerApprovals: [String]
    /// `browser_approvals` (state.rs) — session ids the user approved for
    /// browser use under Ask. Absent ⇒ empty.
    let browserApprovals: [String]
    /// `mcp_worktree_access` (state.rs) — whether sessions may create
    /// Unpeel-managed worktrees. Absent ⇒ false.
    let mcpWorktreeAccess: Bool
    /// `mcp_auto_add_browser_screenshots` (state.rs) — whether Browser MCP
    /// screenshots go straight into the Session gallery. Absent ⇒ true.
    let mcpAutoAddBrowserScreenshots: Bool
    /// `native_preset_overlay_migrated` — set by the one-time fold of the
    /// UserDefaults preset overlay into this file. Once true, the `presets`
    /// array (its order included) is the single preset truth for every UI
    /// and the legacy overlay keys are never read again. Absent ⇒ false.
    let nativePresetOverlayMigrated: Bool
    /// `session_sort_modes` (state.rs) — per-group session sort override,
    /// project/group/worktree id → "date" (recently updated first). Absent
    /// id ⇒ custom (the manual drag order). Absent ⇒ empty.
    let sessionSortModes: [String: String]
    /// `auto_stop_archive_minutes` (state.rs) — stop-and-archive sessions
    /// continuously idle this long. Shared with the TUI. nil = key absent
    /// (⇒ the opt-out default applies); explicit 0 = off.
    let autoStopArchiveMinutes: Int?
    /// `sidebar_stopped_limit` (state.rs) — compatibility key for how many
    /// naturally stopped and archived rows combined each sidebar group
    /// previews. Shared with the TUI. nil = key absent (⇒ the default of five
    /// applies); explicit 0 = no inactive preview.
    let sidebarStoppedLimit: Int?
    /// `session_title_mode` (state.rs) — what drives automatic session
    /// titles: "agent" (default; the runtime's own OSC 0/2 terminal titles,
    /// semantic runtimes only), "first_prompt", or "off". Shared with the
    /// TUI and read live by every session host. Absent or unknown ⇒ the
    /// default, mirroring the Rust serde(other) contract.
    let sessionTitleMode: SessionTitleMode
    /// `profile_display_name` / `profile_avatar` — the Unpeel Link profile:
    /// the nickname and emoji avatar presence surfaces show for this person.
    /// The TUI edits the same keys (Settings ▸ Remote ▸ Unpeel Link). Absent
    /// ⇒ empty; surfaces fall back to the device name.
    let profileDisplayName: String
    let profileAvatar: String

    enum CodingKeys: String, CodingKey {
        case projects
        case pinnedSessions = "pinned_sessions"
        case presets
        case codeEditor = "code_editor"
        case theme
        case mcpOrchestrators = "mcp_orchestrators"
        case mcpBlockedProjects = "mcp_blocked_projects"
        case setupCompleted = "setup_completed"
        case mcpDefaultAccess = "mcp_default_access"
        case browserDefaultAccess = "browser_default_access"
        case browserSettings = "browser_settings"
        case transcriptSettings = "transcript_settings"
        case mcpNonChildWriteAccess = "mcp_nonchild_write_access"
        case mcpWriteApprovals = "mcp_write_approvals"
        case mcpAppOpenApprovals = "mcp_app_open_approvals"
        case appPresentations = "app_presentations"
        case computerDefaultAccess = "computer_default_access"
        case computerApprovals = "computer_approvals"
        case browserApprovals = "browser_approvals"
        case mcpWorktreeAccess = "mcp_worktree_access"
        case mcpAutoAddBrowserScreenshots = "mcp_auto_add_browser_screenshots"
        case nativePresetOverlayMigrated = "native_preset_overlay_migrated"
        case sessionSortModes = "session_sort_modes"
        case autoStopArchiveMinutes = "auto_stop_archive_minutes"
        case sidebarStoppedLimit = "sidebar_stopped_limit"
        case sessionTitleMode = "session_title_mode"
        case profileDisplayName = "profile_display_name"
        case profileAvatar = "profile_avatar"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        projects = try container.decode([Project].self, forKey: .projects)
        pinnedSessions = ((try? container.decodeIfPresent(
            PinnedSessionsFile.self, forKey: .pinnedSessions
        )) ?? nil)?.byProject ?? [:]
        presets = ((try? container.decodeIfPresent(
            [GlobalPresetFile].self, forKey: .presets
        )) ?? nil ?? []).filter { $0.projectID == nil }.map(\.runtime)
        let editor = ((try? container.decodeIfPresent(String.self, forKey: .codeEditor))
            ?? nil ?? "").trimmingCharacters(in: .whitespaces)
        codeEditor = editor.isEmpty ? "code" : editor
        theme = ((try? container.decodeIfPresent(String.self, forKey: .theme))
            ?? nil).flatMap(ThemePreference.init(rawValue:))
        let rawOrchestrators = ((try? container.decodeIfPresent(
            [String: McpGrantFile].self, forKey: .mcpOrchestrators
        )) ?? nil) ?? [:]
        mcpOrchestrators = rawOrchestrators.mapValues { $0.grant }
        mcpDefaultAccess = ((try? container.decodeIfPresent(
            McpGrantFile.self, forKey: .mcpDefaultAccess
        )) ?? nil)?.grant ?? .default
        mcpBlockedProjects = ((try? container.decodeIfPresent(
            [String].self, forKey: .mcpBlockedProjects
        )) ?? nil) ?? []
        setupCompleted = ((try? container.decodeIfPresent(
            Bool.self, forKey: .setupCompleted
        )) ?? nil) ?? false
        // Absent ⇒ the shipped default (on). An explicit malformed value still
        // parses to off — junk never silently *grants*, only absence defaults.
        browserDefaultAccess = BrowserAccess.fromStateString(
            ((try? container.decodeIfPresent(String.self, forKey: .browserDefaultAccess)) ?? nil)
                ?? "on"
        )
        browserSettings = ((try? container.decodeIfPresent(
            BrowserSettings.self, forKey: .browserSettings
        )) ?? nil) ?? BrowserSettings()
        transcriptSettings = ((try? container.decodeIfPresent(
            TranscriptSettings.self, forKey: .transcriptSettings
        )) ?? nil) ?? TranscriptSettings()
        mcpNonChildWriteAccess = McpNonChildWriteAccess.fromStateString(
            (try? container.decodeIfPresent(String.self, forKey: .mcpNonChildWriteAccess)) ?? nil
        )
        mcpWriteApprovals = ((try? container.decodeIfPresent(
            [String: [String]].self, forKey: .mcpWriteApprovals
        )) ?? nil) ?? [:]
        mcpAppOpenApprovals = ((try? container.decodeIfPresent(
            [String: [String]].self, forKey: .mcpAppOpenApprovals
        )) ?? nil) ?? [:]
        appPresentations = (try? container.decodeIfPresent(
            AppPresentationsFile.self, forKey: .appPresentations
        )) ?? nil
        // Absent ⇒ ask (the shipped default); explicit junk parses to off so
        // a malformed value never silently broadens access.
        if let rawComputerAccess = ((try? container.decodeIfPresent(
            String.self, forKey: .computerDefaultAccess
        )) ?? nil) {
            computerDefaultAccess = ComputerAccess.fromStateString(rawComputerAccess)
        } else {
            computerDefaultAccess = .ask
        }
        computerApprovals = ((try? container.decodeIfPresent(
            [String].self, forKey: .computerApprovals
        )) ?? nil) ?? []
        browserApprovals = ((try? container.decodeIfPresent(
            [String].self, forKey: .browserApprovals
        )) ?? nil) ?? []
        mcpWorktreeAccess = ((try? container.decodeIfPresent(
            Bool.self, forKey: .mcpWorktreeAccess
        )) ?? nil) ?? false
        mcpAutoAddBrowserScreenshots = ((try? container.decodeIfPresent(
            Bool.self, forKey: .mcpAutoAddBrowserScreenshots
        )) ?? nil) ?? true
        nativePresetOverlayMigrated = ((try? container.decodeIfPresent(
            Bool.self, forKey: .nativePresetOverlayMigrated
        )) ?? nil) ?? false
        sessionSortModes = ((try? container.decodeIfPresent(
            [String: String].self, forKey: .sessionSortModes
        )) ?? nil) ?? [:]
        autoStopArchiveMinutes = (try? container.decodeIfPresent(
            Int.self, forKey: .autoStopArchiveMinutes
        )) ?? nil
        sidebarStoppedLimit = (try? container.decodeIfPresent(
            Int.self, forKey: .sidebarStoppedLimit
        )) ?? nil
        sessionTitleMode = SessionTitleMode(
            rawValue: ((try? container.decodeIfPresent(
                String.self, forKey: .sessionTitleMode
            )) ?? nil) ?? ""
        ) ?? .agent
        profileDisplayName = ((try? container.decodeIfPresent(
            String.self, forKey: .profileDisplayName
        )) ?? nil) ?? ""
        profileAvatar = ((try? container.decodeIfPresent(
            String.self, forKey: .profileAvatar
        )) ?? nil) ?? ""
    }
}

/// App-wide transcript rendering options (state.rs `TranscriptSettings`). The
/// host reads these from app-state.json when building the Markdown transcript
/// for the native "Copy Transcript" action and the Sessions MCP
/// `read_transcript` tool, so writes go through the same field-preserving path
/// as the access grants.
struct TranscriptSettings: Equatable, Codable {
    /// Include user/prompt turns.
    var includeUser = true
    /// Include assistant/agent turns.
    var includeAssistant = true
    /// Include reasoning/thinking turns.
    var includeReasoning = false
    /// Include tool call + tool result turns.
    var includeTools = false
    /// Include file-change and diff blocks.
    var includeFileChanges = true
    /// Include plan-update blocks.
    var includePlanUpdates = true
    /// Start the Markdown with a session-info header: title, Unpeel session
    /// id (a Sessions MCP target), CLI, model, and command.
    var includeSessionInfo = true
    /// Most-recent entries to render; 0 = whole conversation.
    var maxEntries = 20

    enum CodingKeys: String, CodingKey {
        case includeUser = "include_user"
        case includeAssistant = "include_assistant"
        case includeReasoning = "include_reasoning"
        case includeTools = "include_tools"
        case includeFileChanges = "include_file_changes"
        case includePlanUpdates = "include_plan_updates"
        case includeSessionInfo = "include_session_info"
        case maxEntries = "max_entries"
    }

    init() {}

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        includeUser = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includeUser
        )) ?? nil) ?? true
        includeAssistant = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includeAssistant
        )) ?? nil) ?? true
        includeReasoning = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includeReasoning
        )) ?? nil) ?? false
        includeTools = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includeTools
        )) ?? nil) ?? false
        includeFileChanges = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includeFileChanges
        )) ?? nil) ?? true
        includePlanUpdates = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includePlanUpdates
        )) ?? nil) ?? true
        includeSessionInfo = ((try? container.decodeIfPresent(
            Bool.self, forKey: .includeSessionInfo
        )) ?? nil) ?? true
        maxEntries = ((try? container.decodeIfPresent(
            Int.self, forKey: .maxEntries
        )) ?? nil) ?? 20
    }
}

/// App-wide Browser MCP engine options (state.rs `BrowserSettings`). The host
/// reads these per tool call from app-state.json, so writes go through the
/// same field-preserving path as the access grants.
struct BrowserSettings: Equatable, Codable {
    /// Show the browser window; false runs the engine headless (screenshots
    /// still work).
    var headed = true
    /// Comma-separated domain allowlist (wildcards like `*.example.com`),
    /// engine-enforced. Empty = all sites.
    var allowedDomains = ""
    /// "project" (default) = one Unpeel-managed browser window/profile per
    /// project tree with a pinned tab per Session; "session" = a separate
    /// ephemeral browser per Session.
    var profileMode = "project"
    /// Custom browser executable path; empty = auto-detect system Chrome.
    var executablePath = ""
    /// Animate a visible pointer to each element the agent clicks/fills.
    /// Only applies while the window is shown (headed).
    var showCursor = true

    enum CodingKeys: String, CodingKey {
        case headed
        case allowedDomains = "allowed_domains"
        case profileMode = "profile_mode"
        case executablePath = "executable_path"
        case showCursor = "show_cursor"
    }

    init() {}

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        headed = ((try? container.decodeIfPresent(Bool.self, forKey: .headed)) ?? nil) ?? true
        allowedDomains = ((try? container.decodeIfPresent(
            String.self, forKey: .allowedDomains
        )) ?? nil) ?? ""
        profileMode = ((try? container.decodeIfPresent(
            String.self, forKey: .profileMode
        )) ?? nil) ?? "project"
        executablePath = ((try? container.decodeIfPresent(
            String.self, forKey: .executablePath
        )) ?? nil) ?? ""
        showCursor = ((try? container.decodeIfPresent(
            Bool.self, forKey: .showCursor
        )) ?? nil) ?? true
    }

    var keepsProjectProfile: Bool { profileMode == "project" }
}

/// Browser MCP access for one session (state.rs `BrowserAccess`): a single
/// on/off axis. Pinned tabs isolate Session control even when a project shares
/// one window/profile. Lenient parse: anything but "on" is off, so a malformed
/// value never silently grants access.
enum BrowserAccess: String, CaseIterable, Identifiable {
    case off
    case ask
    // Serialized as "on" for wire compatibility with pre-Ask builds; shown
    // as "Allow" in UI.
    case on

    var id: String { rawValue }

    var label: String {
        switch self {
        case .off: return "Off"
        case .ask: return "Ask each session"
        case .on: return "Allow"
        }
    }

    var detail: String {
        switch self {
        case .off: return "No browser tools for any session."
        case .ask: return "Each session's first browser action asks you once."
        case .on: return "Every session can drive its own pinned browser tab."
        }
    }

    static func fromStateString(_ raw: String) -> BrowserAccess {
        switch raw.trimmingCharacters(in: .whitespaces).lowercased() {
        case "on", "allow": return .on
        case "ask": return .ask
        default: return .off
        }
    }
}

/// Computer MCP access (state.rs `ComputerAccess`): the app-wide mode for the
/// cua-driver-backed `computer` domain. Unlike the browser (isolated per
/// session), computer use reads and drives the user's real apps, so the
/// shipped default is `ask` — a one-time per-session approval alert. Lenient
/// parse: an explicit unknown value is `off` (junk never broadens access);
/// absence defaults to `ask` at the decode site.
enum ComputerAccess: String, CaseIterable, Identifiable {
    case off
    case ask
    case allow

    var id: String { rawValue }

    var label: String {
        switch self {
        case .off: return "Off"
        case .ask: return "Ask each session"
        case .allow: return "Allow"
        }
    }

    var detail: String {
        switch self {
        case .off: return "No computer control for any session."
        case .ask: return "Each session's first computer action asks you once."
        case .allow: return "All sessions can control this Mac without asking."
        }
    }

    static func fromStateString(_ raw: String) -> ComputerAccess {
        switch raw.trimmingCharacters(in: .whitespaces).lowercased() {
        case "ask": return .ask
        case "allow": return .allow
        default: return .off
        }
    }
}

// MARK: - app-sessions/<id>/manifest.json

struct SessionInfo: Codable, Hashable {
    let id: String
    let projectID: String
    let label: String?
    let command: String?
    let createdAt: Int64?
    /// Host-authenticated human identity that owns this Session. Older
    /// manifests omit it and resolve to the local Host owner during rescan.
    let ownerPrincipalID: String?
    /// Optional audit provenance for the independently revocable device that
    /// initiated creation. Access follows the principal, never this device.
    let createdByDeviceID: String?
    /// Preset selected at creation, when one was used.
    let sourcePresetID: String?
    /// True once the user renamed the session (state.rs custom_title);
    /// restart carries it over so auto-titling stays suppressed.
    let customTitle: Bool?
    /// Set when the session runs in a git worktree instead of the project
    /// root (state.rs worktree_path/worktree_branch); restart re-spawns
    /// inside the worktree.
    let worktreePath: String?
    let worktreeBranch: String?
    /// Set when this session was spawned by another session via Sessions MCP.
    let parentSessionID: String?
    let spawnedBy: String?
    let role: String?
    let task: String?

    enum CodingKeys: String, CodingKey {
        case id
        case projectID = "project_id"
        case label, command
        case createdAt = "created_at"
        case ownerPrincipalID = "owner_principal_id"
        case createdByDeviceID = "created_by_device_id"
        case sourcePresetID = "source_preset_id"
        case customTitle = "custom_title"
        case worktreePath = "worktree_path"
        case worktreeBranch = "worktree_branch"
        case parentSessionID = "parent_session_id"
        case spawnedBy = "spawned_by"
        case role, task
    }
}

/// The live, display-only runtime observation persisted by the Host. This is
/// intentionally narrower than the full runtime record: native only needs the
/// current identity to select sidebar/activity presentation, while relaunch
/// actions continue to use `SessionInfo.command` and the stable launch binding.
struct HostedRuntimeObservation: Decodable {
    let id: String?
    /// Observed process identity (pid + kernel start time) — combined with
    /// `id` it identifies WHICH agent process is foreground, so a hook latch
    /// left by a previous run in the same reusable shell can be dropped when
    /// a new process appears. The start time matters because pids recycle
    /// fast under agent load.
    let pid: UInt32?
    let pidStartedAt: UInt64?

    enum CodingKeys: String, CodingKey { case id, pid, pidStartedAt }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try? container.decode(String.self, forKey: .id)
        pid = try? container.decode(UInt32.self, forKey: .pid)
        pidStartedAt = try? container.decode(UInt64.self, forKey: .pidStartedAt)
    }
}

struct HostedSessionRuntime: Decodable {
    let currentObservation: HostedRuntimeObservation?

    enum CodingKeys: String, CodingKey { case currentObservation }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        currentObservation = try? container.decode(
            HostedRuntimeObservation.self, forKey: .currentObservation
        )
    }
}

/// Host-resolved installed Unpeel App identity (`active_app` in the
/// manifest). The Host stamps it at spawn for App-launched sessions and
/// maintains it from foreground observation, so native renders third-party
/// App branding (name, tint) as data without a compiled catalog entry.
/// Identity/presentation only — hook lifecycle events remain the sole source
/// of busy/idle/attention.
struct HostedAppIdentity: Codable, Hashable, Sendable {
    let id: String
    let name: String
    /// `#RRGGBB`, validated Host-side.
    let tint: String?
    let spinnerTint: String?

    enum CodingKeys: String, CodingKey {
        case id, name, tint
        case spinnerTint = "spinner_tint"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? id
        tint = try container.decodeIfPresent(String.self, forKey: .tint)
        spinnerTint = try container.decodeIfPresent(String.self, forKey: .spinnerTint)
    }

    init(id: String, name: String, tint: String?, spinnerTint: String?) {
        self.id = id
        self.name = name
        self.tint = tint
        self.spinnerTint = spinnerTint
    }

    var tintColorHex: Int? {
        Self.parseHex(tint) ?? Self.parseHex(spinnerTint)
    }

    var spinnerTintColorHex: Int? {
        Self.parseHex(spinnerTint) ?? Self.parseHex(tint)
    }

    private static func parseHex(_ value: String?) -> Int? {
        guard let value, value.hasPrefix("#"), value.count == 7 else { return nil }
        return Int(value.dropFirst(), radix: 16)
    }
}

struct HostedSessionManifest: Decodable {
    let session: SessionInfo
    let state: String
    /// Last semantic manifest write. While `state == "running"` this also
    /// advances for host heartbeats and must NOT be treated as user/session
    /// activity; once the host writes `state == "exited"`, it is the final
    /// lifecycle event timestamp used by Recent ordering.
    let updatedAt: UInt64
    let pid: Int32?
    /// Kernel-reported start time (ms since epoch) of the process `pid`
    /// refers to, captured by the host when the pid was recorded. Kill paths
    /// compare it against the live process's start time before signaling —
    /// pids recycle fast under agent load, so a stale manifest's pid can
    /// point at an unrelated live process. Older manifests omit it; identity
    /// is then unprovable and no force-kill may be escalated from it.
    let pidStartedAt: UInt64?
    let hostBuildID: String?
    let hostProtocolVersion: Int?
    let hasBeenWrittenTo: Bool
    let providerSessionID: String?
    let providerTranscriptPath: String?
    /// Host-validated runtime-owned storage. Native treats this as opaque and
    /// only removes it after re-validating that it remains beneath UNPEEL_HOME.
    let managedStoragePath: String?
    /// Provider-verified markers for a failed precise resume. Native only
    /// performs the bounded output scan; it owns no provider text patterns.
    let resumeFailureMarkers: [String]
    /// Current Host-observed foreground runtime. Optional at every level so
    /// manifests written by older Hosts continue to decode as generic
    /// terminals.
    let runtime: HostedSessionRuntime?
    /// Host-resolved installed Unpeel App identity; nil for non-App sessions
    /// and manifests from older Hosts.
    let activeApp: HostedAppIdentity?
    /// Stable managed-runtime launch generation. Direct launches start at one
    /// and in-place agent restarts increment it; blank terminals stay at zero.
    let runtimeLaunchGeneration: UInt64
    /// True after a same-terminal resume command was committed but before the
    /// Host observes its foreground runtime. Older manifests default false.
    let runtimeLaunchPending: Bool
    let runtimeLaunchedAt: UInt64?
    /// Byte boundary in output.bin captured immediately before the current
    /// managed runtime was submitted. Resume-failure detection must start
    /// here so an older process generation cannot trigger a false match.
    let runtimeLaunchOutputOffset: UInt64
    /// Whether Unpeel automatically registered the unified MCP client with the
    /// provider while the Sessions domain was enabled. This is provider-setup
    /// evidence, separate from the saved domain grant; older manifests default
    /// false.
    let mcpClientRegistered: Bool
    /// Whether automatic provider setup included the Browser domain. Manual
    /// MCP registration and domain authorization are separate facts.
    let browserClientRegistered: Bool
    /// True while the session's visible screen looks like an agent-drawn select
    /// menu waiting for a keyboard choice (Claude/Codex numbered prompts). The
    /// host edge-writes it because these menus fire no lifecycle hook; the
    /// sidebar turns it into the attention badge. Older manifests default false.
    let menuPromptActive: Bool
    /// Wall-clock ms when the host last saw the parsed screen TEXT change.
    /// Idle repaint loops that redraw identical content (grok's idle
    /// animation) advance output.bin forever but never advance this stamp,
    /// so it is the preferred activity signal for busy heuristics and
    /// recency. Nil on manifests from older hosts (fall back to output.bin).
    let screenChangedAt: UInt64?
    /// Loopback URLs this session's processes currently serve as browsable
    /// pages (printed on screen, then HTTP-probed live by the host). The host
    /// removes entries when the server stops answering; older manifests
    /// predate the field and default empty.
    let detectedLocalURLs: [String]
    /// Working directory the PTY was launched in. Older manifests predate
    /// the read (the host has always written it); nil ⇒ project-path fallback.
    let cwd: String?

    enum CodingKeys: String, CodingKey {
        case session, state, pid, cwd
        case updatedAt = "updated_at"
        case pidStartedAt = "pid_started_at"
        case hostBuildID = "host_build_id"
        case hostProtocolVersion = "host_protocol_version"
        case hasBeenWrittenTo = "has_been_written_to"
        case providerSessionID = "provider_session_id"
        case providerTranscriptPath = "provider_transcript_path"
        case managedStoragePath = "managed_storage_path"
        case resumeFailureMarkers = "resume_failure_markers"
        case runtime
        case activeApp = "active_app"
        case runtimeLaunchGeneration = "runtime_launch_generation"
        case runtimeLaunchPending = "runtime_launch_pending"
        case runtimeLaunchedAt = "runtime_launched_at"
        case runtimeLaunchOutputOffset = "runtime_launch_output_offset"
        case mcpClientRegistered = "mcp_client_registered"
        case browserClientRegistered = "browser_client_registered"
        case menuPromptActive = "menu_prompt_active"
        case screenChangedAt = "screen_changed_at"
        case detectedLocalURLs = "detected_local_urls"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        session = try container.decode(SessionInfo.self, forKey: .session)
        state = try container.decode(String.self, forKey: .state)
        updatedAt = try container.decodeIfPresent(UInt64.self, forKey: .updatedAt) ?? 0
        pid = try container.decodeIfPresent(Int32.self, forKey: .pid)
        pidStartedAt = try container.decodeIfPresent(UInt64.self, forKey: .pidStartedAt)
        hostBuildID = try container.decodeIfPresent(String.self, forKey: .hostBuildID)
        hostProtocolVersion = try container.decodeIfPresent(Int.self, forKey: .hostProtocolVersion)
        // Older manifests predate this field; keep their prior restart
        // behavior instead of treating them as definitely unresumable.
        hasBeenWrittenTo = try container.decodeIfPresent(
            Bool.self, forKey: .hasBeenWrittenTo
        ) ?? true
        providerSessionID = try container.decodeIfPresent(
            String.self, forKey: .providerSessionID
        )
        providerTranscriptPath = try container.decodeIfPresent(
            String.self, forKey: .providerTranscriptPath
        )
        managedStoragePath = try container.decodeIfPresent(
            String.self, forKey: .managedStoragePath
        )
        resumeFailureMarkers = try container.decodeIfPresent(
            [String].self, forKey: .resumeFailureMarkers
        ) ?? []
        runtime = try? container.decode(HostedSessionRuntime.self, forKey: .runtime)
        activeApp = try? container.decode(HostedAppIdentity.self, forKey: .activeApp)
        runtimeLaunchGeneration = try container.decodeIfPresent(
            UInt64.self, forKey: .runtimeLaunchGeneration
        ) ?? 0
        runtimeLaunchPending = try container.decodeIfPresent(
            Bool.self, forKey: .runtimeLaunchPending
        ) ?? false
        runtimeLaunchedAt = try container.decodeIfPresent(
            UInt64.self, forKey: .runtimeLaunchedAt
        )
        runtimeLaunchOutputOffset = try container.decodeIfPresent(
            UInt64.self, forKey: .runtimeLaunchOutputOffset
        ) ?? 0
        mcpClientRegistered = try container.decodeIfPresent(
            Bool.self, forKey: .mcpClientRegistered
        ) ?? false
        browserClientRegistered = try container.decodeIfPresent(
            Bool.self, forKey: .browserClientRegistered
        ) ?? false
        menuPromptActive = try container.decodeIfPresent(
            Bool.self, forKey: .menuPromptActive
        ) ?? false
        screenChangedAt = try container.decodeIfPresent(
            UInt64.self, forKey: .screenChangedAt
        )
        detectedLocalURLs = try container.decodeIfPresent(
            [String].self, forKey: .detectedLocalURLs
        ) ?? []
        cwd = try container.decodeIfPresent(String.self, forKey: .cwd)
    }
}

// MARK: - App-level session row model

enum SessionStatus: String, Codable, Hashable, Sendable {
    case starting
    case busy
    case idle
    case attention
    case exited
}

/// Product-facing status derived from the lower-level lifecycle state plus
/// observation/unread state. `done` is intentionally not a provider-reported
/// lifecycle state; it means the session settled while the user was away.
enum SessionActivityStatus: String, Codable, Hashable {
    case starting
    case working
    case blocked
    case done
    case idle
    case exited
}

struct SessionEntry: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let projectID: String
    let label: String
    let command: String
    let createdAt: Int64
    /// Immutable Session owner. Display names are resolved through the
    /// principal directory instead of being copied into each manifest.
    var ownerPrincipalID: String? = nil
    var createdByDeviceID: String? = nil
    var sourcePresetID: String? = nil
    var status: SessionStatus
    /// Volatile Host observation used only for live presentation in this
    /// first slice. Restart/fork/context and hook authority continue to use
    /// the stable launch binding (`command`).
    var activeRuntimeID: String? = nil
    /// Host-resolved installed Unpeel App identity (manifest `active_app`,
    /// or wire `activeAppID`/`activeAppName`/`activeAppTintHex` for remote
    /// rows). Presentation data plus hook-authority evidence: an App reports
    /// its own lifecycle, so its presence marks the session hook-owned.
    var activeApp: HostedAppIdentity? = nil
    /// Host-owned transition after accepting Resume Agent and before the new
    /// foreground runtime is observable. Suppresses duplicate launch actions.
    var runtimeLaunchPending: Bool = false
    /// Protocol implemented by this hosted PTY. A Session may survive an app
    /// update while still running an older Host without in-place restart.
    var hostProtocolVersion: Int? = nil
    /// Restart provenance (SessionInfo parity): manifest custom_title plus
    /// the worktree the session runs in (nil = project root).
    var customTitle: Bool = false
    var worktreePath: String?
    var worktreeBranch: String?
    var spawnedBy: String? = nil
    var role: String? = nil
    var task: String? = nil
    var providerTranscriptPath: String? = nil
    /// Host-owned proof that this exact managed launch produced a provider
    /// conversation (or provider-owned per-Session storage). Runtime support
    /// alone is deliberately insufficient for Archive/Resume.
    var hasResumableState: Bool = false
    /// `project-override.json` marker: the user filed this session under a
    /// plain organizational group. Display + ordering only — the manifest's
    /// projectID stays the launch truth. Applied at tree build, where a stale
    /// target falls back to `projectID` (including legacy worktree targets).
    var projectOverrideID: String? = nil
    /// Canonical Recent timestamp: creation as the floor, then the latest
    /// command-aware real activity, plus the final manifest update once the
    /// host has exited. Running manifest heartbeats are deliberately absent.
    /// Local scans populate it from session artifacts; remote rows receive
    /// the Host-computed value through `updatedAtUnixMs`.
    var lifecycleAtMs: Int64? = nil
    /// Working directory the Session's PTY was launched in: the manifest
    /// `cwd` for local scans, the wire `cwd` for Host-published rows. Seeds
    /// the pane's cwd (`UnpeelStore.paneWorkingDirectory`); nil on rows from
    /// older Hosts, which fall back to the project path.
    var cwd: String? = nil

    var isLive: Bool { status != .exited }
    var isAttachable: Bool { status != .starting && status != .exited }

    /// Existing native presentation is command-driven. Translate a
    /// process-observed runtime ID into its legacy command head at this
    /// single boundary so sidebar icons/colors work for agents started later
    /// from a blank terminal without changing the stable launch command.
    var presentationCommand: String {
        guard isLive,
              let observed = RuntimePresentation.command(for: activeRuntimeID)
        else { return command }
        return observed
    }

    /// Copy Transcript is a semantic agent action, not a terminal fallback.
    /// Use the stable launch binding rather than `presentationCommand`: a
    /// runtime observed inside a blank shell may borrow the agent's logo, but
    /// it does not gain that runtime's transcript adapter or conversation
    /// binding.
    var supportsTranscriptCopy: Bool {
        guard let runtime = UnpeelRuntimeCatalog.runtime(command: command),
              runtime.kind == .agent
        else { return false }
        return runtime.capabilities.contains(.transcript)
    }

    func activityStatus(unread: Bool) -> SessionActivityStatus {
        switch status {
        case .starting:
            return .starting
        case .busy:
            return .working
        case .attention:
            return .blocked
        case .idle:
            return unread ? .done : .idle
        case .exited:
            return unread ? .done : .exited
        }
    }

    /// Relative age like the Svelte sidebar: "now / 5m / 3h / 2d". Date-
    /// sorted rows pass their lifecycle stamp so the age describes the same
    /// event that placed the row; custom-order rows retain creation age.
    func ageString(since timestampMs: Int64? = nil, now: Date = Date()) -> String {
        let timestamp = timestampMs ?? createdAt
        let date = Date(timeIntervalSince1970: TimeInterval(timestamp) / 1000)
        let seconds = max(0, now.timeIntervalSince(date))
        if seconds < 60 { return "now" }
        let minutes = Int(seconds / 60)
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        if hours < 24 { return "\(hours)h" }
        return "\(hours / 24)d"
    }
}

enum SessionOwnership {
    /// Compatibility identity for the one human who owns today's Host. A
    /// future Link account principal can replace it for newly created
    /// Sessions without changing the persisted Session contract.
    static func hostOwnerPrincipalID(hostID: String) -> String {
        "host-owner:\(hostID)"
    }
}

enum RuntimePresentation {
    static func command(for runtimeID: String?) -> String? {
        UnpeelRuntimeCatalog.presentationCommand(forRuntimeID: runtimeID)
    }
}

// MARK: - Sidebar tree node

/// A top-level project plus its sessions and worktree children, ready for
/// the sidebar to render.
struct ProjectNode: Codable, Identifiable, Hashable, Sendable {
    let project: Project
    var sessions: [SessionEntry]
    var worktrees: [ProjectNode]

    var id: String { project.id }

    var hasAnyContent: Bool { !sessions.isEmpty || !worktrees.isEmpty }
}

/// One row in a project's regular (non-pinned) sidebar section: a session
/// or a child folder (group / worktree). Custom order interleaves them.
enum SidebarMixedItem: Identifiable, Equatable {
    case session(SessionEntry)
    case folder(ProjectNode)

    var id: String {
        switch self {
        case .session(let session): return session.id
        case .folder(let node): return node.id
        }
    }
}
