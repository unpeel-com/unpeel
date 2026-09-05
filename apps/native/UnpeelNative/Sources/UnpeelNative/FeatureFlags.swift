import Foundation

/// A user-facing experimental feature, toggleable in Settings ▸ Experimental.
///
/// Adding a new experiment is a single entry in `all` below: it automatically
/// gets a toggle row in the Experimental tab and an `isEnabled` check you can
/// gate UI on. Persistence is a native UserDefaults overlay (never
/// app-state.json), keyed by `defaultsKey`; optional environment overrides are
/// dev escape hatches that force-enable the feature when an env var == "1".
struct ExperimentalFeature: Identifiable, Hashable {
    /// Stable id; also the UserDefaults key suffix. Never rename once shipped.
    let key: String
    let title: String
    let summary: String
    let defaultsKey: String
    let envOverride: String?
    let legacyEnvOverrides: [String]
    let defaultOn: Bool

    var id: String { key }

    init(
        key: String,
        title: String,
        summary: String,
        envOverride: String? = nil,
        legacyEnvOverrides: [String] = [],
        defaultOn: Bool = false
    ) {
        self.key = key
        self.title = title
        self.summary = summary
        self.defaultsKey = "unpeel.experimental.\(key)"
        self.envOverride = envOverride
        self.legacyEnvOverrides = legacyEnvOverrides
        self.defaultOn = defaultOn
    }

    var envOverrides: [String] {
        [envOverride].compactMap { $0 } + legacyEnvOverrides
    }
}

extension ExperimentalFeature {
    /// Run sessions in isolated git worktrees so multiple agents can work the
    /// same repo in parallel. Gates the project-menu worktree controls, the
    /// inline worktree folder rows, and Settings ▸ Worktrees.
    static let worktrees = ExperimentalFeature(
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
    static let sessionsMcp = ExperimentalFeature(
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
    static let workspaces = ExperimentalFeature(
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

    /// Computer Use MCP: agent sessions can read app windows and drive them
    /// in the background through the embedded cua-driver engine. Gates the
    /// Settings ▸ Computer tab, the engine daemon, and whether new sessions
    /// launch with the `computer` domain advertised.
    static let computerUse = ExperimentalFeature(
        key: "computerUse",
        title: "Computer use",
        summary: "Development only. Let agent sessions control this Host's desktop apps in the "
            + "background: read a window's UI elements, take screenshots, click, and type — "
            + "without moving your cursor or stealing focus. By default each "
            + "session asks you once before its first action. That prompt coordinates "
            + "agents; it is not isolation from same-user shell code. Adds the Computer "
            + "settings tab in development builds only.",
        envOverride: "UNPEEL_DEV_COMPUTER_USE",
        defaultOn: false
    )

    /// Browser MCP: agent sessions get an isolated real browser. Gates the
    /// Settings ▸ Browser tab and whether new sessions launch with the
    /// `browser` domain advertised.
    static let browserMcp = ExperimentalFeature(
        key: "browserMcp",
        title: "Browser use",
        summary: "Let agent sessions drive a real browser — open pages, click, "
            + "fill forms, and take screenshots. Each session gets its own "
            + "isolated browser with no access to your normal browser profile. Browser "
            + "access prompts are cooperation controls, not a sandbox against commands "
            + "running as your macOS user. Adds the Browser settings tab.",
        envOverride: "UNPEEL_DEV_BROWSER_MCP",
        defaultOn: true
    )

    /// Remote workspaces in the released app (decided 2026-09-02): the Host
    /// picker, Share This Mac…, Add Workspace… ▸ Nearby/code and SSH. Direct is
    /// bearer-authenticated plaintext meant for LAN/VPN; Link carries the
    /// encrypted path off-network. Off hides the picker again at the next launch.
    static let remoteWorkspaces = ExperimentalFeature(
        key: "remoteWorkspaces",
        title: "Remote workspaces",
        summary: "Add and control workspaces on other machines — pair another Mac, a "
            + "headless `unpeel serve` box, or an SSH host — and share this Mac with "
            + "other devices. Direct connections are for your own network or VPN; "
            + "Unpeel Link carries the encrypted path when you are away.",
        envOverride: "UNPEEL_DEV_REMOTE_WORKSPACES",
        defaultOn: true
    )

    /// Everything shown in Settings ▸ Experimental, in display order.
    static let all: [ExperimentalFeature] = [
        .remoteWorkspaces, .worktrees, .sessionsMcp, .browserMcp, .computerUse,
        .workspaces,
    ]
}

enum UnpeelFeatureFlags {
    /// Computer Use currently relies on an unrestricted same-UID daemon that
    /// inherits the app's TCC grants. Until hosted sessions have a kernel-
    /// enforced broker boundary, it is a development-build facility only.
    static var computerUseAvailable: Bool {
        computerUseAvailable(infoDictionary: Bundle.main.infoDictionary)
    }

    /// Pure form used by containment tests. The marker is baked into dev
    /// bundles by build-app.sh; missing, false, or a wrong type fails closed.
    static func computerUseAvailable(infoDictionary: [String: Any]?) -> Bool {
        infoDictionary?["UnpeelDevelopmentBuild"] as? Bool == true
    }

    static func isAvailable(_ feature: ExperimentalFeature) -> Bool {
        isAvailable(feature, developmentBuild: computerUseAvailable)
    }

    /// Whether THIS Controller may operate the selected Host's computer use
    /// (decision D2, `docs/plans/computer-use-release.md`): it follows what
    /// the Host advertises in its bootstrap (`computerUseAvailable`), never
    /// this app's build flavor. A Linux Host running `unpeel serve` in a
    /// desktop session launders no privilege, so a release Mac app may drive
    /// it; the Mac's own desktop daemon stays behind `computerUseAvailable`
    /// (development builds only) regardless of this value. Absent or false
    /// fails closed.
    static func computerUseControllable(hostAdvertisesAvailability: Bool?) -> Bool {
        hostAdvertisesAvailability == true
    }

    /// Availability independent of Bundle.main, so tests cover the production
    /// boundary without relying on the Swift test runner's Info.plist.
    static func isAvailable(
        _ feature: ExperimentalFeature, developmentBuild: Bool
    ) -> Bool {
        feature != .computerUse || developmentBuild
    }

    static var availableExperimentalFeatures: [ExperimentalFeature] {
        ExperimentalFeature.all.filter(isAvailable)
    }

    /// Whether an experimental feature is currently enabled — env override
    /// first (dev escape hatch), then this workspace's own stored
    /// preference, then the default workspace's value (Decision 4
    /// generalized, 2026-08-23: a local workspace with no setting of its own
    /// inherits the default's from the shared `.standard` domain — same
    /// filesystem only), then the feature's built-in default.
    static func isEnabled(_ feature: ExperimentalFeature) -> Bool {
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
    static func hasOwnSetting(_ feature: ExperimentalFeature) -> Bool {
        AppDefaults.shared.object(forKey: feature.defaultsKey) != nil
    }

    /// Decision 4's revert for experimental flags: drop every own value so
    /// this workspace inherits the default workspace's flags again.
    static func revertToInheritedBaseline() {
        for feature in ExperimentalFeature.all {
            AppDefaults.shared.removeObject(forKey: feature.defaultsKey)
        }
    }

    /// Persist a user preference for an experimental feature.
    static func setEnabled(_ enabled: Bool, for feature: ExperimentalFeature) {
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
