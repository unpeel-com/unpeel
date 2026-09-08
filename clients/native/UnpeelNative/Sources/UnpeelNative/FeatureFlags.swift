import Foundation

/// A user-facing optional feature, toggleable in Settings ▸ Features.
///
/// Adding a feature is a single entry in `all` below: it automatically gets
/// a toggle row in the Features tab and an `isEnabled` check you can gate UI
/// on. A feature is either shipped (the plain Features list) or
/// `experimental` (the tab's Experimental section: still being shaped, may
/// change or disappear between releases). Graduating one is flipping that
/// flag; the toggle, key, and gates stay. Persistence is a native
/// UserDefaults overlay (never app-state.json), keyed by `defaultsKey` —
/// the `unpeel.experimental.` prefix is the shipped spelling for every
/// feature, graduated or not; optional environment overrides are dev escape
/// hatches that force-enable the feature when an env var == "1".
struct AppFeature: Identifiable, Hashable {
    /// Stable id; also the UserDefaults key suffix. Never rename once shipped.
    let key: String
    let title: String
    let summary: String
    let defaultsKey: String
    let envOverride: String?
    let legacyEnvOverrides: [String]
    let defaultOn: Bool
    /// Still being shaped: listed under the Features tab's Experimental
    /// section instead of the shipped list.
    let experimental: Bool

    var id: String { key }

    init(
        key: String,
        title: String,
        summary: String,
        envOverride: String? = nil,
        legacyEnvOverrides: [String] = [],
        defaultOn: Bool = false,
        experimental: Bool = false
    ) {
        self.key = key
        self.title = title
        self.summary = summary
        self.defaultsKey = "unpeel.experimental.\(key)"
        self.envOverride = envOverride
        self.legacyEnvOverrides = legacyEnvOverrides
        self.defaultOn = defaultOn
        self.experimental = experimental
    }

    var envOverrides: [String] {
        [envOverride].compactMap { $0 } + legacyEnvOverrides
    }
}

extension AppFeature {
    /// Run sessions in isolated git worktrees so multiple agents can work the
    /// same repo in parallel. Gates the project-menu worktree controls, the
    /// inline worktree folder rows, and Settings ▸ Worktrees.
    static let worktrees = AppFeature(
        key: "worktrees",
        title: "Git worktrees",
        summary: "Run sessions in an isolated git worktree of a project so multiple "
            + "agents can work the same repo in parallel without touching each other's "
            + "files. Adds worktree controls to the project menu, sidebar, and the "
            + "Worktrees settings tab.",
        envOverride: "UNPEEL_DEV_WORKTREES",
        defaultOn: true
    )

    /// Sessions MCP: agent sessions can read other sessions and request write
    /// access to explicit targets. Gates the Settings ▸ Sessions use tab and
    /// whether new sessions launch with the MCP client injected.
    static let sessionsMcp = AppFeature(
        key: "sessionsMcp",
        title: "Sessions use",
        summary: "Let an agent session see your other sessions: it can read them all, "
            + "and asks before writing to another session unless you already approved "
            + "that pair. These are cooperation controls, not a sandbox "
            + "against commands running as your macOS user. Adds the Sessions settings "
            + "tab. Applies when a session starts, so already-running sessions pick it "
            + "up after a restart.",
        envOverride: "UNPEEL_DEV_SESSIONS_MCP",
        defaultOn: true
    )

    /// Workspaces: use additional, fully isolated Unpeel homes on this Mac
    /// (own sessions, projects, settings, and phone pairing identity).
    /// Gates the Settings ▸ Workspaces tab. The persisted key is deliberately
    /// still `profiles`: shipped experimental-feature keys are immutable.
    static let workspaces = AppFeature(
        key: "profiles",
        title: "Workspaces",
        summary: "Use extra, fully separate workspaces on this Mac — each "
            + "workspace has its own sessions, projects, presets, settings, and "
            + "pairs with your phone as its own workspace. Adds the Workspaces "
            + "settings tab.",
        envOverride: "UNPEEL_DEV_WORKSPACES",
        legacyEnvOverrides: ["UNPEEL_DEV_PROFILES"],
        defaultOn: true
    )

    /// Legacy preference identity retained for decoding saved settings.
    static let computerUse = AppFeature(
        key: "computerUse",
        title: "Computer use",
        summary: "Unpeel computer use has been retired.",
        experimental: true
    )

    /// Browser MCP: agent sessions get an isolated real browser. Gates the
    /// Settings ▸ Browser tab and whether new sessions launch with the
    /// `browser` domain advertised. Still experimental (2026-09-08): the
    /// engine pin, login persistence, and takeover story are moving.
    static let browserMcp = AppFeature(
        key: "browserMcp",
        title: "Browser use",
        summary: "Let agent sessions drive a real browser — open pages, click, "
            + "fill forms, and take screenshots. Each session gets its own "
            + "isolated browser with no access to your normal browser profile. Browser "
            + "access prompts are cooperation controls, not a sandbox against commands "
            + "running as your macOS user. Adds the Browser settings tab.",
        envOverride: "UNPEEL_DEV_BROWSER_MCP",
        defaultOn: true,
        experimental: true
    )

    /// Remote workspaces in the released app (decided 2026-09-02): the Host
    /// picker, Share This Mac…, Add Workspace… ▸ Nearby/code and SSH. Direct is
    /// bearer-authenticated plaintext meant for LAN/VPN; Link carries the
    /// encrypted path off-network. Off hides the picker again at the next launch.
    static let remoteWorkspaces = AppFeature(
        key: "remoteWorkspaces",
        title: "Remote workspaces",
        summary: "Add and control workspaces on other machines — pair another Mac, a "
            + "headless `unpeel serve` box, or an SSH host — and share this Mac with "
            + "other devices. Direct connections are for your own network or VPN; "
            + "Unpeel Link carries the encrypted path when you are away.",
        envOverride: "UNPEEL_DEV_REMOTE_WORKSPACES",
        defaultOn: true
    )

    /// Everything shown in Settings ▸ Features, in display order (shipped
    /// features first; the panel then groups the experimental ones under
    /// their own section). Remote workspaces, Git worktrees, Sessions use,
    /// and Workspaces graduated on 2026-09-08; Browser use stays experimental.
    static let all: [AppFeature] = [
        .remoteWorkspaces, .worktrees, .sessionsMcp, .workspaces,
        .browserMcp,
    ]

    /// Header copy for the Features tab's Experimental section, shared by the
    /// local, per-workspace, and remote Host panels.
    static let experimentalSectionDescription =
        "Early features that are still being shaped. They can change or "
        + "disappear between releases. Turn one off here if it gets in the way."
}

enum UnpeelFeatureFlags {
    // Kept for old saved settings; this feature is retired in every build.
    static var computerUseAvailable: Bool { false }

    static func computerUseAvailable(infoDictionary: [String: Any]?) -> Bool { false }

    static func computerUseControllable(hostAdvertisesAvailability: Bool?) -> Bool { false }

    static func isAvailable(_ feature: AppFeature) -> Bool {
        feature != .computerUse
    }

    static func isAvailable(_ feature: AppFeature, developmentBuild: Bool) -> Bool {
        isAvailable(feature)
    }

    /// Every feature this build offers a toggle for, in display order.
    static var availableFeatures: [AppFeature] {
        AppFeature.all.filter(isAvailable)
    }

    /// The shipped features: the Features tab's plain list.
    static var availableShippedFeatures: [AppFeature] {
        availableFeatures.filter { !$0.experimental }
    }

    /// The features still marked experimental: the tab's Experimental section.
    static var availableExperimentalFeatures: [AppFeature] {
        availableFeatures.filter(\.experimental)
    }

    /// Whether a feature is currently enabled — env override
    /// first (dev escape hatch), then this workspace's own stored
    /// preference, then the default workspace's value (Decision 4
    /// generalized, 2026-08-23: a local workspace with no setting of its own
    /// inherits the default's from the shared `.standard` domain — same
    /// filesystem only), then the feature's built-in default.
    static func isEnabled(_ feature: AppFeature) -> Bool {
        guard isAvailable(feature) else { return false }
        if feature.envOverrides.contains(where: {
            ProcessInfo.processInfo.environment[$0] == "1"
        }) {
            return true
        }
        if let own = AppDefaults.shared.object(forKey: feature.defaultsKey) as? Bool {
            return own
        }
        if !UnpeelWorkspaceContext.isDefaultInstance,
           let inherited = UserDefaults.standard.object(forKey: feature.defaultsKey) as? Bool {
            return inherited
        }
        return feature.defaultOn
    }

    /// Whether this workspace records its OWN value for the feature — the
    /// revert-to-default button's enablement.
    static func hasOwnSetting(_ feature: AppFeature) -> Bool {
        AppDefaults.shared.object(forKey: feature.defaultsKey) != nil
    }

    /// Decision 4's revert for feature flags: drop every own value so this
    /// workspace inherits the default workspace's flags again.
    static func revertToInheritedBaseline() {
        for feature in AppFeature.all {
            AppDefaults.shared.removeObject(forKey: feature.defaultsKey)
        }
    }

    /// Persist a user preference for a feature.
    static func setEnabled(_ enabled: Bool, for feature: AppFeature) {
        guard isAvailable(feature) else { return }
        AppDefaults.shared.set(enabled, forKey: feature.defaultsKey)
    }

    static var mobileRemoteControlEnabled: Bool {
        if ProcessInfo.processInfo.environment["UNPEEL_DEV_MOBILE_REMOTE"] == "1" {
            return true
        }
        let key = "unpeel.dev.mobileRemoteControl"
        guard AppDefaults.shared.object(forKey: key) != nil else {
            return true
        }
        return AppDefaults.shared.bool(forKey: key)
    }

    /// Mac-as-client: connect this Unpeel to another Unpeel's remote server
    /// and attach to its sessions. Experimental; pairs with the Rust-side
    /// UNPEEL_REMOTE_ATTACH=1 gate on the attach CLI.
    static var remoteUnpeelClientEnabled: Bool {
        if ProcessInfo.processInfo.environment["UNPEEL_REMOTE_ATTACH"] == "1" {
            return true
        }
        return AppDefaults.shared.bool(forKey: "unpeel.dev.remoteUnpeelClient")
    }
}

/// Desktop workspace switching is a local feature, even though its transport
/// reuses the Host client stack. The remote Host picker remains development-
/// only; tying both surfaces to that gate stranded release users in a newly
/// launched workspace after they quit its separate app instance.
enum WorkspaceFeature {
    static var pickerEnabled: Bool {
        pickerEnabled(
            localWorkspacesEnabled: UnpeelFeatureFlags.isEnabled(.workspaces),
            remoteHostPickerEnabled: RemoteHostFeature.pickerEnabled
        )
    }

    /// Pure form keeps the release boundary testable without depending on the
    /// test runner's Info.plist.
    nonisolated static func pickerEnabled(
        localWorkspacesEnabled: Bool,
        remoteHostPickerEnabled: Bool
    ) -> Bool {
        localWorkspacesEnabled || remoteHostPickerEnabled
    }
}
