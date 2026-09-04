use crate::integrations;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "system".into()
}

fn default_color_scheme() -> String {
    "default".into()
}

fn default_code_editor() -> String {
    "code".into()
}

fn default_workspace_id() -> String {
    "personal".into()
}

fn default_project_sort_order() -> u32 {
    0
}

pub fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Initial sidebar/titlebar label for a newly launched Session. Agent
/// presets keep their command as the provisional label; a blank terminal
/// shows the folder it opened in until its first submitted command replaces
/// the label through the Host's ordinary prompt auto-title path.
pub fn initial_session_label(command: &str, cwd: &str) -> String {
    if !command.trim().is_empty() {
        return command.to_string();
    }

    let label = abbreviate_home_path(cwd, dirs::home_dir().as_deref());
    if label.trim().is_empty() {
        "Terminal".into()
    } else {
        label
    }
}

fn abbreviate_home_path(path: &str, home: Option<&Path>) -> String {
    let path = Path::new(path);
    let Some(home) = home else {
        return path.to_string_lossy().into_owned();
    };
    if path == home {
        return "~".into();
    }
    match path.strip_prefix(home) {
        Ok(relative) if !relative.as_os_str().is_empty() => {
            format!("~/{}", relative.to_string_lossy())
        }
        _ => path.to_string_lossy().into_owned(),
    }
}

pub fn pinned_sidebar_session_key(session_id: &str) -> String {
    format!("session:{session_id}")
}

fn deserialize_pinned_sessions_by_project<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, Vec<PinnedSidebarSession>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawPinnedSessions {
        Legacy(Vec<PinnedSidebarSession>),
        ByProject(HashMap<String, Vec<PinnedSidebarSession>>),
        /// Anything else — including a shape a future version invents.
        /// This arm is why an unrecognised `pinned_sessions` costs the
        /// reader its pins instead of every project in the file. The value
        /// is deliberately discarded: we cannot interpret it, only survive
        /// it.
        Unknown(#[allow(dead_code)] serde_json::Value),
    }

    let raw = Option::<RawPinnedSessions>::deserialize(deserializer)?;
    let mut grouped = HashMap::<String, Vec<PinnedSidebarSession>>::new();

    match raw {
        None => {}
        Some(RawPinnedSessions::Legacy(pins)) => {
            for pin in pins {
                grouped.entry(pin.project_id.clone()).or_default().push(pin);
            }
        }
        Some(RawPinnedSessions::ByProject(pins)) => {
            grouped = pins;
        }
        // A shape we don't recognise: keep the rest of the document.
        Some(RawPinnedSessions::Unknown(_)) => {}
    }

    Ok(grouped)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    /// Plain sidebar groups may be pinned above their parent's ordinary
    /// mixed rows. Kept optional so older state files and clients ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<u64>,
    #[serde(default = "default_workspace_id")]
    pub workspace_id: String,
    #[serde(default)]
    pub parent_project_id: Option<String>,
    #[serde(default = "default_project_sort_order")]
    pub sort_order: u32,
    /// Virtual folders are organizational containers with no filesystem path.
    #[serde(default)]
    pub is_folder: bool,
    /// Set when this project is a git-worktree workspace of its parent project.
    #[serde(default)]
    pub worktree_branch: Option<String>,
    /// When true, the sidebar offers the slide-in workspaces view for this project.
    #[serde(default)]
    pub workspaces_enabled: bool,
}

/// Reach of an Unpeel Sessions MCP caller: how far a permitted caller can
/// see and control other sessions. The user-facing model only exposes roles;
/// reach stays project-bound for new writes, with `Global` retained so older
/// state files and tests remain readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpScope {
    /// Session-control tools are disabled entirely for this caller.
    Off,
    /// A session may only see/control sessions in its own project tree.
    /// This is the default: not all chats can talk to all chats.
    #[default]
    Project,
    /// Any session may control any other. Per-project blocks still apply on top.
    Global,
}

impl McpScope {
    /// Lenient parse of the persisted string form; unknown/missing values
    /// fall back to the safe default (`Project`) rather than erroring, so a
    /// malformed setting never silently widens access.
    pub fn from_state_str(value: &str) -> McpScope {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => McpScope::Off,
            "global" => McpScope::Global,
            _ => McpScope::Project,
        }
    }

    pub fn as_state_str(self) -> &'static str {
        match self {
            McpScope::Off => "off",
            McpScope::Project => "project",
            McpScope::Global => "global",
        }
    }
}

/// The capability a session has on Unpeel Sessions MCP. The product exposes
/// two roles: `Read` by default, and `Write` for creating/driving/closing
/// sessions. `Off` is internal for unknown callers and old disabled state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpRole {
    /// No session tools at all. This is not a user-facing role anymore; it is
    /// used for unknown callers and explicit test fixtures.
    Off,
    /// Read in-reach sessions. The default for sessions not listed in the
    /// override map.
    #[default]
    Read,
    /// Read, create, write to, and close in-reach sessions.
    Write,
}

impl McpRole {
    /// Lenient parse of the persisted string form. Legacy `member` maps to
    /// `Read`; legacy `orchestrator` maps to `Write`; legacy `off` maps back to
    /// the default read role so old "No access" state does not leave a hidden
    /// disabled session after the two-role migration.
    pub fn from_state_str(value: &str) -> McpRole {
        match value.trim().to_ascii_lowercase().as_str() {
            "write" | "writer" | "orchestrator" => McpRole::Write,
            _ => McpRole::Read,
        }
    }

    pub fn as_state_str(self) -> &'static str {
        match self {
            McpRole::Off => "off",
            McpRole::Read => "read",
            McpRole::Write => "write",
        }
    }
}

/// A per-session Unpeel Sessions MCP grant: a `role` (capability) plus a `reach`
/// (how far it sees/controls). Stored in `AppState.mcp_orchestrators` keyed by
/// session id; sessions absent from the map use the default ([`McpRole::Read`]
/// at project reach). Persists as `{ "role": ..., "reach": ... }` and accepts
/// legacy forms for migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpGrant {
    pub role: McpRole,
    /// Reach as `Project` or `Global`. Never `Off`; the role carries the
    /// "no access" state instead.
    pub reach: McpScope,
}

impl Default for McpGrant {
    fn default() -> Self {
        McpGrant {
            role: McpRole::Read,
            reach: McpScope::Project,
        }
    }
}

impl McpGrant {
    /// The effective per-call reach this grant evaluates against: `Off` when the
    /// role is `Off`, otherwise the stored reach.
    pub fn effective_scope(self) -> McpScope {
        match self.role {
            McpRole::Off => McpScope::Off,
            _ => self.reach,
        }
    }

    /// Parse a reach string into the current fixed project reach. Older state
    /// may contain `global`; the two-role model intentionally stops exposing
    /// or preserving that hidden scope.
    fn parse_reach(value: &str) -> McpScope {
        let _ = value;
        McpScope::Project
    }
}

impl Serialize for McpGrant {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("McpGrant", 2)?;
        state.serialize_field("role", self.role.as_state_str())?;
        state.serialize_field("reach", self.reach.as_state_str())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for McpGrant {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            /// Legacy form: a bare reach string meant the elevated role.
            Legacy(String),
            Full {
                role: Option<String>,
                reach: Option<String>,
            },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Legacy(reach) => McpGrant {
                role: McpRole::Write,
                reach: McpGrant::parse_reach(&reach),
            },
            Raw::Full { role, reach } => McpGrant {
                role: role
                    .as_deref()
                    .map(McpRole::from_state_str)
                    .unwrap_or_default(),
                reach: reach
                    .as_deref()
                    .map(McpGrant::parse_reach)
                    .unwrap_or(McpScope::Project),
            },
        })
    }
}

/// What happens when a session tries to write (send_text/send_keys) to any
/// other session. Stored app-wide under the historical compatibility key
/// `AppState.mcp_nonchild_write_access` and re-read by the Sessions MCP host
/// per tool call, so changes apply live. Sidebar groups are organizational and
/// never bypass this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpNonChildWriteAccess {
    /// Ask the user to approve the caller→target pair the first time; an
    /// approval is remembered in `AppState.mcp_write_approvals` until either
    /// session is removed.
    #[default]
    Ask,
    /// Never allow writes to another session.
    Deny,
    /// Allow any session to write to any other session without asking.
    Allow,
}

impl McpNonChildWriteAccess {
    /// Lenient parse of the persisted string form; unknown/missing values fall
    /// back to `Ask` (the shipped default).
    pub fn from_state_str(value: &str) -> McpNonChildWriteAccess {
        match value.trim().to_ascii_lowercase().as_str() {
            "deny" => McpNonChildWriteAccess::Deny,
            "allow" => McpNonChildWriteAccess::Allow,
            _ => McpNonChildWriteAccess::Ask,
        }
    }

    pub fn as_state_str(self) -> &'static str {
        match self {
            McpNonChildWriteAccess::Ask => "ask",
            McpNonChildWriteAccess::Deny => "deny",
            McpNonChildWriteAccess::Allow => "allow",
        }
    }
}

/// A per-session Unpeel Browser MCP grant: whether the session's agent may use
/// the Unpeel-managed browser automation engine. Deliberately a single on/off
/// axis: each Session owns one engine daemon and pinned tab even when its
/// project shares the surrounding browser window/profile.
///
/// Defaults to `On`: the engine uses only Unpeel-managed project profiles (no
/// access to the user's own browser data), and Unpeel agents already run with
/// full shell access. Settings ▸ Browser ▸ Off is the master disable;
/// personal-profile or live-user-browser modes would still require opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrowserAccess {
    /// No browser tools for this session.
    Off,
    /// Each session's first browser action asks the user once (remembered in
    /// `browser_approvals`, same lifecycle as computer approvals).
    Ask,
    /// The session's agent gets the browser tools without prompting. The
    /// default — browser data stays in Unpeel-managed project scope.
    /// Serialized as `"on"` for wire compatibility with pre-Ask builds.
    #[default]
    On,
}

impl BrowserAccess {
    /// Lenient parse of the persisted string form; unknown/missing values fall
    /// back to `Off` so a malformed setting never silently grants access.
    /// Accepts `"allow"` as a synonym for `"on"`.
    pub fn from_state_str(value: &str) -> BrowserAccess {
        match value.trim().to_ascii_lowercase().as_str() {
            "on" | "allow" => BrowserAccess::On,
            "ask" => BrowserAccess::Ask,
            _ => BrowserAccess::Off,
        }
    }

    pub fn as_state_str(self) -> &'static str {
        match self {
            BrowserAccess::Off => "off",
            BrowserAccess::Ask => "ask",
            BrowserAccess::On => "on",
        }
    }
}

/// App-wide Unpeel Browser MCP engine options (`AppState.browser_settings`),
/// read per tool call by the `__browser_mcp__` server so changes apply live.
/// All fields have conservative defaults so an absent object behaves like the
/// shipped configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSettings {
    /// Show the browser window (headed). Off runs the engine headless; agents
    /// still screenshot fine, the user just doesn't see the window.
    #[serde(default = "default_browser_headed")]
    pub headed: bool,
    /// Comma-separated domain allowlist (wildcards like `*.example.com`),
    /// enforced by the engine for navigation, sub-resources, and WebSockets.
    /// Empty = all sites allowed.
    #[serde(default)]
    pub allowed_domains: String,
    /// "project" (default) = one persistent browser window/profile per
    /// project tree under `~/.unpeel/browser/profiles/`; every Session owns a
    /// pinned tab in that window and shares its logins — never the user's own
    /// Chrome profile. "session" = a separate ephemeral browser per Session.
    #[serde(default = "default_browser_profile_mode")]
    pub profile_mode: String,
    /// Custom browser executable path (Brave/Edge/Chromium). Empty =
    /// auto-detect the system Chrome.
    #[serde(default)]
    pub executable_path: String,
    /// Animate a visible pointer to each element the agent clicks/fills, so a
    /// human watching the headed window can follow the agent's actions. Only
    /// applies when `headed` is on (invisible + pure latency otherwise).
    #[serde(default = "default_browser_show_cursor")]
    pub show_cursor: bool,
}

fn default_browser_show_cursor() -> bool {
    true
}

fn default_browser_headed() -> bool {
    true
}

fn default_browser_profile_mode() -> String {
    "project".to_string()
}

impl Default for BrowserSettings {
    fn default() -> Self {
        BrowserSettings {
            headed: default_browser_headed(),
            allowed_domains: String::new(),
            profile_mode: default_browser_profile_mode(),
            executable_path: String::new(),
            show_cursor: default_browser_show_cursor(),
        }
    }
}

/// Computer Use access for the Unpeel Computer MCP domain (cua-driver engine).
/// Unlike the browser — isolated per session by construction — computer use
/// sees the user's real screen and drives the real mouse/keyboard, so the
/// default is `Ask`: each session needs a one-time user approval (remembered
/// in `computer_approvals`, pruned/carried with the session). `Allow` skips
/// the prompt app-wide; `Off` is the master disable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ComputerAccess {
    /// No computer tools for any session.
    Off,
    /// Every session's first computer action asks the user for approval.
    #[default]
    Ask,
    /// All sessions get the computer tools without prompting.
    Allow,
}

impl ComputerAccess {
    /// Lenient parse of the persisted string form. An *absent* field decodes
    /// to the serde default (`Ask`); an explicit-but-unrecognized value falls
    /// back to `Off` so a malformed setting never silently broadens access
    /// (same fail-closed asymmetry as `BrowserAccess::from_state_str`).
    pub fn from_state_str(value: &str) -> ComputerAccess {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => ComputerAccess::Ask,
            "allow" => ComputerAccess::Allow,
            _ => ComputerAccess::Off,
        }
    }

    pub fn as_state_str(self) -> &'static str {
        match self {
            ComputerAccess::Off => "off",
            ComputerAccess::Ask => "ask",
            ComputerAccess::Allow => "allow",
        }
    }
}

/// App-wide Computer MCP engine options (`AppState.computer_settings`), read
/// per tool call so changes apply live. Deliberately minimal: there is only
/// one real screen, so no profile/headed axes exist here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComputerSettings {
    /// Comma/space-separated allowlist of app names or bundle ids the agent
    /// may target (guardrail, not a sandbox — launches are checked directly;
    /// pid-addressed actions resolve the pid through the engine's running-app
    /// list and fail closed when it can't be identified). Empty = all apps.
    #[serde(default)]
    pub allowed_apps: String,
}

/// App-wide transcript rendering options (`AppState.transcript_settings`),
/// used to build the Markdown transcript for both the native "Copy Transcript"
/// action and the Sessions MCP `read_transcript` tool (as its defaults). All
/// fields have defaults so an absent object behaves like the shipped
/// configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptSettings {
    /// Include user/prompt turns.
    #[serde(default = "default_true")]
    pub include_user: bool,
    /// Include assistant/agent turns.
    #[serde(default = "default_true")]
    pub include_assistant: bool,
    /// Include reasoning/thinking turns.
    #[serde(default)]
    pub include_reasoning: bool,
    /// Include tool call + tool result turns.
    #[serde(default)]
    pub include_tools: bool,
    /// Include file-change and diff blocks.
    #[serde(default = "default_true")]
    pub include_file_changes: bool,
    /// Include plan-update blocks.
    #[serde(default = "default_true")]
    pub include_plan_updates: bool,
    /// Start the Markdown with a session-info header: title, Unpeel session
    /// id (usable as a Sessions MCP target), CLI, model, and command.
    #[serde(default = "default_true")]
    pub include_session_info: bool,
    /// Maximum number of most-recent entries to render; `0` = whole conversation.
    #[serde(default = "default_transcript_max_entries")]
    pub max_entries: usize,
}

fn default_transcript_max_entries() -> usize {
    20
}

impl Default for TranscriptSettings {
    fn default() -> Self {
        TranscriptSettings {
            include_user: true,
            include_assistant: true,
            include_reasoning: false,
            include_tools: false,
            include_file_changes: true,
            include_plan_updates: true,
            include_session_info: true,
            max_entries: default_transcript_max_entries(),
        }
    }
}

/// Topmost project in the parent chain (the worktree group root), bounded
/// against cycles. Projects not present in `projects` (e.g. client
/// overlay-only records) resolve to themselves.
pub fn project_tree_root(projects: &[Project], project_id: &str) -> String {
    let mut current = project_id.to_string();
    for _ in 0..16 {
        match projects.iter().find(|project| project.id == current) {
            Some(project) => match project.parent_project_id.as_deref() {
                Some(parent) => current = parent.to_string(),
                None => return current,
            },
            None => return current,
        }
    }
    current
}

/// Whether two projects belong to the same worktree group. Falls back to id
/// equality when parentage is unknown (overlay-only projects), so distinct
/// project ids are never treated as the same tree.
pub fn projects_in_same_tree(projects: &[Project], a: &str, b: &str) -> bool {
    a == b || project_tree_root(projects, a) == project_tree_root(projects, b)
}

/// Whether the configured scope permits a caller in `caller_project` to
/// see/control a session in `target_project`.
pub fn mcp_scope_permits(
    projects: &[Project],
    scope: McpScope,
    caller_project: Option<&str>,
    target_project: &str,
) -> bool {
    match scope {
        McpScope::Off => false,
        McpScope::Global => true,
        McpScope::Project => match caller_project {
            Some(caller) => projects_in_same_tree(projects, caller, target_project),
            None => false,
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub command: String,
    /// If Some, only applies to this project
    pub project_id: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub quick_launch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tag {
    pub id: String,
    pub label: String,
    pub color: String,
    #[serde(default)]
    pub sort_order: u32,
    /// If Some, only applies to this project
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub project_id: String,
    pub label: String,
    #[serde(default)]
    pub custom_title: bool,
    pub command: String,
    /// Session creation time in Unix milliseconds, used for deterministic ordering.
    #[serde(default)]
    pub created_at: u64,
    /// Stable human/principal that owns this Session. This is stamped by the
    /// Host from authenticated creation context, never accepted from a remote
    /// create body. Older manifests omit it and resolve to this Host's owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_principal_id: Option<String>,
    /// Optional audit provenance for the independently revocable device that
    /// initiated creation. Authorization follows `owner_principal_id`, not
    /// this device id, so all of one person's devices can reach their Session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_device_id: Option<String>,
    /// Host-owned preset id used for the launch. The Session remains owned by
    /// its creating principal; using a shared preset never transfers preset
    /// or Session ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_preset_id: Option<String>,
    #[serde(default)]
    pub tag_id: Option<String>,
    /// Set when the session runs in a git worktree instead of the project root.
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub worktree_branch: Option<String>,
    /// Legacy parent id retained only so manifests written by older builds
    /// remain decodable. Current grouping and MCP permissions ignore it.
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// Launch provenance retained for forks and older manifests; it does not
    /// affect group membership or permissions.
    #[serde(default)]
    pub spawned_by: Option<String>,
    /// Optional legacy launch label; ignored by grouping and permissions.
    #[serde(default)]
    pub role: Option<String>,
    /// Optional legacy task summary; ignored by grouping and permissions.
    #[serde(default)]
    pub task: Option<String>,
}

/// Compatibility principal for today's single-owner Hosts. The Host id keeps
/// this namespace stable and collision-free until Link account principals can
/// replace it for newly created Sessions.
pub fn host_owner_principal_id(host_id: &str) -> String {
    format!("host-owner:{host_id}")
}

/// Attribution ids are opaque, but they are persisted and projected over the
/// remote protocol. Bound them and reject invisible/padded identities at the
/// final Host boundary without imposing an account-provider-specific syntax.
pub const SESSION_ATTRIBUTION_ID_MAX_BYTES: usize = 256;

pub fn valid_session_attribution_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= SESSION_ATTRIBUTION_ID_MAX_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastSession {
    pub command: String,
    pub label: String,
    #[serde(default)]
    pub custom_title: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinnedSidebarSession {
    pub key: String,
    pub project_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub pinned_at: u64,
}

/// What drives a session's automatic title (the label shown until the user
/// renames the row — a custom title permanently wins in every mode).
/// Stored app-wide as `AppState.session_title_mode` and re-read from disk by
/// each session host when a candidate title arrives, so changes apply live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionTitleMode {
    /// One-shot title from the first submitted prompt.
    FirstPrompt,
    /// No automatic titling — sessions keep their command label.
    Off,
    /// Follow the runtime's own `OSC 0/2` terminal titles while a
    /// `semantic_terminal_title` runtime is in the foreground; falls back to
    /// the first-prompt title until the first such title arrives.
    /// Unknown future values decode here (`serde(other)` must sit on the
    /// last variant) so a newer writer costs the older reader this field,
    /// never the document.
    #[default]
    #[serde(other)]
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    // Every field here defaults. This file is written by whichever Unpeel
    // is newest, read by whichever is oldest: a key that appears, vanishes,
    // or is renamed must cost the reader that field, never the document.
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub active_project_id: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub active_workspace_id: Option<String>,
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Set by the native app's one-time fold of its UserDefaults preset
    /// overlay into this file. Once true, `presets` — array order included —
    /// is the single preset truth for every UI, and the legacy overlay
    /// presets (`com.unpeel.native` defaults) are stale copies that must be
    /// ignored.
    #[serde(default)]
    pub native_preset_overlay_migrated: bool,
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// project_id -> active tab session_id
    #[serde(default)]
    pub active_tabs: HashMap<String, String>,
    /// Sessions persisted across app restarts (for resume)
    #[serde(default)]
    pub saved_sessions: Vec<SessionInfo>,
    #[serde(default, deserialize_with = "deserialize_pinned_sessions_by_project")]
    pub pinned_sessions: HashMap<String, Vec<PinnedSidebarSession>>,
    /// "dark" | "light" | "system"
    #[serde(default = "default_theme")]
    pub theme: String,
    /// "default" | "purple" | "blue" | "moss" | "clay"
    #[serde(default = "default_color_scheme")]
    pub color_scheme: String,
    /// Code editor command (e.g. "code", "cursor", "zed", "idea")
    #[serde(default = "default_code_editor")]
    pub code_editor: String,
    /// project_id -> last session info (for quick resume when all sessions are closed)
    #[serde(default)]
    pub last_sessions: HashMap<String, LastSession>,
    /// Whether the first-run setup wizard has been completed
    #[serde(default)]
    pub setup_completed: bool,
    /// Per-session Unpeel Sessions MCP access overrides, keyed by session id.
    /// The value is an [`McpGrant`] (role + reach). Sessions not listed use
    /// `mcp_default_access`. The JSON key is kept as `mcp_orchestrators` for
    /// on-disk back-compat; legacy bare-string values still decode (to
    /// Write grants).
    #[serde(default)]
    pub mcp_orchestrators: HashMap<String, McpGrant>,
    /// The default Unpeel Sessions MCP grant for any session without an explicit
    /// override in `mcp_orchestrators`. Defaults to [`McpGrant::default`]
    /// (Read at project reach). The user can raise this app-wide to the write
    /// role.
    #[serde(default)]
    pub mcp_default_access: McpGrant,
    /// App-wide policy for every Sessions MCP write to another session: ask
    /// the user (default), deny outright, or always allow. The JSON key keeps
    /// its historical `nonchild` spelling for compatibility.
    #[serde(default)]
    pub mcp_nonchild_write_access: McpNonChildWriteAccess,
    /// User-approved write pairs, `caller session id → approved
    /// target session ids`. Written by the native app when the user answers
    /// the approval prompt (and from Settings ▸ Sessions MCP revoke); read per
    /// tool call by the Sessions MCP host. Pruned with the sessions and
    /// re-pointed across restarts.
    #[serde(default)]
    pub mcp_write_approvals: HashMap<String, Vec<String>>,
    /// Per-session Unpeel Browser MCP access overrides, keyed by session id.
    /// Deviations-only: sessions not listed use `browser_default_access`.
    #[serde(default)]
    pub browser_access: HashMap<String, BrowserAccess>,
    /// The default Unpeel Browser MCP grant for any session without an explicit
    /// override in `browser_access`. Defaults to [`BrowserAccess::On`]; setting
    /// it to Off in Settings ▸ Browser is the master disable.
    #[serde(default)]
    pub browser_default_access: BrowserAccess,
    /// App-wide Browser MCP engine options (window visibility, site rules,
    /// browsing-data mode, custom executable).
    #[serde(default)]
    pub browser_settings: BrowserSettings,
    /// Session ids approved for browser use under `BrowserAccess::Ask`.
    /// Same lifecycle as `computer_approvals`: pruned with the session,
    /// carried across restarts.
    #[serde(default)]
    pub browser_approvals: Vec<String>,
    /// Whether sessions may create Unpeel-managed worktrees through the
    /// sessions tool (Settings ▸ Sessions use). Default off; session
    /// creation stays user-only regardless — this grants git/project prep,
    /// not agent spawning.
    #[serde(default)]
    pub mcp_worktree_access: bool,
    /// Whether Browser MCP screenshots are published into the Session gallery
    /// by default. Explicit screenshot calls may override this (the phone's
    /// typed screenshot request always asks for gallery publication).
    #[serde(default = "default_true")]
    pub mcp_auto_add_browser_screenshots: bool,
    /// The app-wide Computer MCP access mode. Defaults to [`ComputerAccess::Ask`]
    /// (per-session approval); Off in Settings ▸ Computer is the master disable.
    #[serde(default)]
    pub computer_default_access: ComputerAccess,
    /// Session ids approved for computer use under `Ask` (the "target" is
    /// always this Mac, so plain ids, not pairs). Pruned with the session and
    /// carried across restarts like `mcp_write_approvals`.
    #[serde(default)]
    pub computer_approvals: Vec<String>,
    /// App-wide Computer MCP engine options (app allowlist).
    #[serde(default)]
    pub computer_settings: ComputerSettings,
    /// App-wide transcript rendering options, shared by the native "Copy
    /// Transcript" action and the Sessions MCP `read_transcript` tool.
    #[serde(default)]
    pub transcript_settings: TranscriptSettings,
    /// Whether an agent-drawn pick menu promotes the Session to Attention.
    /// The session host always detects the screen marker; each Host frontend
    /// applies this workspace setting when it projects activity.
    #[serde(default = "default_true")]
    pub menu_attention_detection: bool,
    /// Per-group session sort override: project/group/worktree id → "date"
    /// (recently updated first). An absent id means custom — the manual
    /// drag order.
    /// A top-level map (not a `Project` field) so desktop-owned
    /// (UserDefaults-only) projects and cwd buckets can carry the mode even
    /// though they never appear in `projects`.
    #[serde(default)]
    pub session_sort_modes: HashMap<String, String>,
    /// Auto-stop-and-archive cutoff in minutes: sessions continuously idle
    /// this long are stopped and archived (the non-destructive Stop verb —
    /// Restore + Restart resumes). Shared knob for the app and the TUI.
    /// None = key absent = on at the default cutoff (1 day, opt-out);
    /// explicit 0 = off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_stop_archive_minutes: Option<u64>,
    /// Compatibility key for how many naturally stopped and archived rows
    /// combined each sidebar group previews. Hidden stopped rows are not filed
    /// or deleted. Shared knob for the app and the TUI. None = key absent =
    /// the default window (5). Explicit 0 = no inactive preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_stopped_limit: Option<u64>,
    /// What drives automatic session titles: the agent's own semantic
    /// terminal titles (default), the first submitted prompt, or nothing.
    /// Shared knob for the app, the TUI, and every session host.
    #[serde(default)]
    pub session_title_mode: SessionTitleMode,
}

/// IDs of built-in global presets in preferred display order.
pub const BUILTIN_PRESET_IDS: &[&str] = integrations::BUILTIN_PRESET_IDS;

pub fn preset_supports_quick_launch(command: &str) -> bool {
    integrations::preset_supports_quick_launch(command)
}

pub fn sanitize_preset_quick_launch(command: &str, quick_launch: bool) -> bool {
    quick_launch && preset_supports_quick_launch(command)
}

pub fn builtin_global_presets() -> Vec<Preset> {
    integrations::builtin_presets()
        .iter()
        .map(|preset| Preset {
            id: preset.id.into(),
            label: preset.label.into(),
            command: preset.command.into(),
            project_id: None,
            enabled: true,
            quick_launch: preset.quick_launch,
        })
        .collect()
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
            active_project_id: None,
            workspaces: vec![Workspace {
                id: "personal".into(),
                name: "Personal".into(),
            }],
            active_workspace_id: Some("personal".into()),
            presets: Vec::new(),
            native_preset_overlay_migrated: false,
            tags: Vec::new(),
            active_tabs: HashMap::new(),
            saved_sessions: Vec::new(),
            pinned_sessions: HashMap::new(),
            theme: "system".into(),
            color_scheme: "default".into(),
            code_editor: "code".into(),
            last_sessions: HashMap::new(),
            setup_completed: false,
            mcp_orchestrators: HashMap::new(),
            mcp_default_access: McpGrant::default(),
            mcp_nonchild_write_access: McpNonChildWriteAccess::default(),
            mcp_write_approvals: HashMap::new(),
            browser_access: HashMap::new(),
            browser_default_access: BrowserAccess::default(),
            browser_settings: BrowserSettings::default(),
            browser_approvals: Vec::new(),
            mcp_worktree_access: false,
            mcp_auto_add_browser_screenshots: true,
            computer_default_access: ComputerAccess::default(),
            computer_approvals: Vec::new(),
            computer_settings: ComputerSettings::default(),
            transcript_settings: TranscriptSettings::default(),
            menu_attention_detection: true,
            session_sort_modes: HashMap::new(),
            auto_stop_archive_minutes: None,
            sidebar_stopped_limit: None,
            session_title_mode: SessionTitleMode::default(),
        }
    }
}

pub fn sort_sessions_newest_first(sessions: &mut [SessionInfo]) {
    sessions.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub fn sort_pinned_sessions_newest_first(sessions: &mut [PinnedSidebarSession]) {
    sessions.sort_by(|a, b| {
        b.pinned_at
            .cmp(&a.pinned_at)
            .then_with(|| a.key.cmp(&b.key))
    });
}

pub fn normalize_pinned_sessions(state: &mut AppState) -> bool {
    let original = state.pinned_sessions.clone();
    let project_ids = state
        .projects
        .iter()
        .map(|project| project.id.as_str())
        .collect::<HashSet<_>>();
    let mut changed = false;
    let fallback_timestamp = current_timestamp_ms();
    let mut normalized = HashMap::with_capacity(state.pinned_sessions.len());

    for (project_id, pins) in std::mem::take(&mut state.pinned_sessions) {
        if !project_ids.contains(project_id.as_str()) {
            changed = true;
            continue;
        }

        let mut seen = HashSet::new();
        let mut normalized_pins = Vec::with_capacity(pins.len());

        for mut pin in pins {
            if pin.project_id != project_id {
                pin.project_id = project_id.clone();
                changed = true;
            }

            let normalized_session_id = pin
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);

            if pin.session_id != normalized_session_id {
                changed = true;
            }

            pin.session_id = normalized_session_id;

            let expected_key = match pin.session_id.as_deref().map(pinned_sidebar_session_key) {
                Some(key) => key,
                None => {
                    changed = true;
                    continue;
                }
            };

            if pin.key != expected_key {
                pin.key = expected_key;
                changed = true;
            }

            if pin.pinned_at == 0 {
                pin.pinned_at = fallback_timestamp;
                changed = true;
            }

            if !seen.insert(pin.key.clone()) {
                changed = true;
                continue;
            }

            normalized_pins.push(pin);
        }

        sort_pinned_sessions_newest_first(&mut normalized_pins);
        if !normalized_pins.is_empty() {
            normalized.insert(project_id, normalized_pins);
        } else {
            changed = true;
        }
    }
    if original != normalized {
        changed = true;
    }
    state.pinned_sessions = normalized;
    changed
}

pub fn normalize_projects(state: &mut AppState) -> bool {
    let original = state.projects.clone();
    let original_index = state
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| (project.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let valid_ids = state
        .projects
        .iter()
        .map(|project| project.id.clone())
        .collect::<HashSet<_>>();
    let mut changed = false;

    for project in &mut state.projects {
        let normalized_parent_id = project
            .parent_project_id
            .as_deref()
            .map(str::trim)
            .filter(|parent_id| {
                !parent_id.is_empty()
                    && *parent_id != project.id.as_str()
                    && valid_ids.contains(*parent_id)
            })
            .map(str::to_string);

        if project.parent_project_id != normalized_parent_id {
            project.parent_project_id = normalized_parent_id;
            changed = true;
        }
    }

    loop {
        let parent_by_id = state
            .projects
            .iter()
            .map(|project| (project.id.clone(), project.parent_project_id.clone()))
            .collect::<HashMap<_, _>>();
        let mut updated_cycle = false;

        for project in &mut state.projects {
            let mut seen = HashSet::from([project.id.clone()]);
            let mut current = project.parent_project_id.clone();

            while let Some(parent_id) = current {
                if !seen.insert(parent_id.clone()) {
                    project.parent_project_id = None;
                    changed = true;
                    updated_cycle = true;
                    break;
                }
                current = parent_by_id.get(&parent_id).cloned().flatten();
            }
        }

        if !updated_cycle {
            break;
        }
    }

    let mut sibling_ids_by_parent = HashMap::<Option<String>, Vec<String>>::new();
    for project in &state.projects {
        sibling_ids_by_parent
            .entry(project.parent_project_id.clone())
            .or_default()
            .push(project.id.clone());
    }

    let project_by_id = state
        .projects
        .iter()
        .map(|project| (project.id.clone(), project.clone()))
        .collect::<HashMap<_, _>>();

    for sibling_ids in sibling_ids_by_parent.values_mut() {
        sibling_ids.sort_by(|left, right| {
            let left_project = project_by_id.get(left).unwrap();
            let right_project = project_by_id.get(right).unwrap();
            left_project
                .sort_order
                .cmp(&right_project.sort_order)
                .then_with(|| {
                    original_index
                        .get(left)
                        .copied()
                        .unwrap_or(usize::MAX)
                        .cmp(&original_index.get(right).copied().unwrap_or(usize::MAX))
                })
                .then_with(|| left.cmp(right))
        });
    }

    for sibling_ids in sibling_ids_by_parent.values() {
        for (index, project_id) in sibling_ids.iter().enumerate() {
            if let Some(project) = state
                .projects
                .iter_mut()
                .find(|project| project.id == *project_id)
            {
                let next_sort_order = index as u32;
                if project.sort_order != next_sort_order {
                    project.sort_order = next_sort_order;
                    changed = true;
                }
            }
        }
    }

    let mut ordered_ids = Vec::with_capacity(state.projects.len());
    let mut visited = HashSet::new();

    fn append_project_branch(
        parent_id: Option<&str>,
        sibling_ids_by_parent: &HashMap<Option<String>, Vec<String>>,
        ordered_ids: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) {
        let key = parent_id.map(str::to_string);
        let Some(sibling_ids) = sibling_ids_by_parent.get(&key) else {
            return;
        };

        for project_id in sibling_ids {
            if !visited.insert(project_id.clone()) {
                continue;
            }
            ordered_ids.push(project_id.clone());
            append_project_branch(
                Some(project_id.as_str()),
                sibling_ids_by_parent,
                ordered_ids,
                visited,
            );
        }
    }

    append_project_branch(None, &sibling_ids_by_parent, &mut ordered_ids, &mut visited);

    if ordered_ids.len() != state.projects.len() {
        let mut remaining = state
            .projects
            .iter()
            .map(|project| project.id.clone())
            .filter(|project_id| !visited.contains(project_id))
            .collect::<Vec<_>>();
        remaining.sort_by_key(|project_id| {
            original_index
                .get(project_id)
                .copied()
                .unwrap_or(usize::MAX)
        });
        ordered_ids.extend(remaining);
    }

    let ordered_positions = ordered_ids
        .iter()
        .enumerate()
        .map(|(index, project_id)| (project_id.clone(), index))
        .collect::<HashMap<_, _>>();

    state.projects.sort_by_key(|project| {
        ordered_positions
            .get(&project.id)
            .copied()
            .unwrap_or(usize::MAX)
    });

    if !changed {
        changed = original != state.projects;
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::{
        abbreviate_home_path, builtin_global_presets, initial_session_label, mcp_scope_permits,
        normalize_pinned_sessions, normalize_projects, projects_in_same_tree,
        valid_session_attribution_id, AppState, McpGrant, McpRole, McpScope, PinnedSidebarSession,
        Project, SessionInfo, SessionTitleMode,
    };
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn initial_session_label_uses_command_or_abbreviated_cwd() {
        assert_eq!(initial_session_label("claude", "/tmp/project"), "claude");
        assert_eq!(
            abbreviate_home_path("/Users/test/Dev/unpeel", Some(Path::new("/Users/test"))),
            "~/Dev/unpeel"
        );
        assert_eq!(
            abbreviate_home_path("/Users/testing/project", Some(Path::new("/Users/test"))),
            "/Users/testing/project"
        );
        assert_eq!(
            abbreviate_home_path("/Users/test", Some(Path::new("/Users/test"))),
            "~"
        );
    }

    fn project(id: &str, parent_project_id: Option<&str>, sort_order: u32) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            path: format!("/tmp/{id}"),
            pinned_at: None,
            workspace_id: "personal".into(),
            parent_project_id: parent_project_id.map(str::to_string),
            sort_order,
            is_folder: false,
            worktree_branch: None,
            workspaces_enabled: false,
        }
    }

    #[test]
    fn scope_off_permits_nothing() {
        assert!(!mcp_scope_permits(&[], McpScope::Off, Some("a"), "a"));
    }

    #[test]
    fn session_ownership_is_additive_for_legacy_manifests() {
        let legacy = serde_json::json!({
            "id": "session-1",
            "project_id": "project-1",
            "label": "Claude",
            "command": "claude",
            "created_at": 1,
        });
        let decoded: SessionInfo = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.owner_principal_id, None);
        assert_eq!(decoded.created_by_device_id, None);
        assert_eq!(decoded.source_preset_id, None);

        let mut owned = decoded;
        owned.owner_principal_id = Some("account:alice".into());
        owned.created_by_device_id = Some("phone-1".into());
        owned.source_preset_id = Some("claude-plan".into());
        let encoded = serde_json::to_value(owned).unwrap();
        assert_eq!(encoded["owner_principal_id"], "account:alice");
        assert_eq!(encoded["created_by_device_id"], "phone-1");
        assert_eq!(encoded["source_preset_id"], "claude-plan");
    }

    #[test]
    fn session_attribution_ids_are_opaque_but_bounded_and_visible() {
        assert!(valid_session_attribution_id("account:alice@example.com"));
        assert!(!valid_session_attribution_id(""));
        assert!(!valid_session_attribution_id(" account:alice"));
        assert!(!valid_session_attribution_id("account:alice\nadmin"));
        assert!(!valid_session_attribution_id(&"a".repeat(257)));
    }

    #[test]
    fn scope_global_permits_any_pair() {
        assert!(mcp_scope_permits(&[], McpScope::Global, Some("a"), "b"));
        // Even with an unknown caller, global is wide open.
        assert!(mcp_scope_permits(&[], McpScope::Global, None, "b"));
    }

    #[test]
    fn scope_project_isolates_distinct_projects_but_groups_worktrees() {
        let repo = project("repo", None, 0);
        let worktree = project("worktree", Some("repo"), 0);
        let other = project("other", None, 1);
        let projects = vec![repo, worktree, other];

        // Same project, and worktree↔parent (same tree): allowed.
        assert!(mcp_scope_permits(
            &projects,
            McpScope::Project,
            Some("repo"),
            "repo"
        ));
        assert!(mcp_scope_permits(
            &projects,
            McpScope::Project,
            Some("repo"),
            "worktree"
        ));
        assert!(mcp_scope_permits(
            &projects,
            McpScope::Project,
            Some("worktree"),
            "repo"
        ));
        // Different project tree: denied.
        assert!(!mcp_scope_permits(
            &projects,
            McpScope::Project,
            Some("repo"),
            "other"
        ));
        // Unknown caller can't be authorized under project scope.
        assert!(!mcp_scope_permits(
            &projects,
            McpScope::Project,
            None,
            "repo"
        ));
    }

    #[test]
    fn same_tree_falls_back_to_id_equality_for_overlay_projects() {
        // Neither id is in the array (overlay-only); only identical ids match.
        assert!(projects_in_same_tree(&[], "native-1", "native-1"));
        assert!(!projects_in_same_tree(&[], "native-1", "native-2"));
    }

    #[test]
    fn mcp_scope_round_trips_through_state_strings() {
        assert_eq!(McpScope::from_state_str("off"), McpScope::Off);
        assert_eq!(McpScope::from_state_str("PROJECT"), McpScope::Project);
        assert_eq!(McpScope::from_state_str("global"), McpScope::Global);
        // Unknown / garbage falls back to the safe default.
        assert_eq!(McpScope::from_state_str("nonsense"), McpScope::Project);
        assert_eq!(McpScope::default(), McpScope::Project);
    }

    #[test]
    fn mcp_role_round_trips_through_state_strings() {
        assert_eq!(McpRole::from_state_str("off"), McpRole::Read);
        assert_eq!(McpRole::from_state_str("read"), McpRole::Read);
        assert_eq!(McpRole::from_state_str("MEMBER"), McpRole::Read);
        assert_eq!(McpRole::from_state_str("write"), McpRole::Write);
        assert_eq!(McpRole::from_state_str("orchestrator"), McpRole::Write);
        // Unknown / garbage falls back to the default access, never silently Off.
        assert_eq!(McpRole::from_state_str("nonsense"), McpRole::Read);
        assert_eq!(McpRole::default(), McpRole::Read);
        assert_eq!(McpRole::Off.as_state_str(), "off");
        assert_eq!(McpRole::Read.as_state_str(), "read");
        assert_eq!(McpRole::Write.as_state_str(), "write");
    }

    #[test]
    fn mcp_grant_decodes_legacy_string_and_object_forms() {
        // Legacy bare-string reach decodes to a Write grant, now project-bound.
        let legacy: McpGrant = serde_json::from_str("\"global\"").unwrap();
        assert_eq!(legacy.role, McpRole::Write);
        assert_eq!(legacy.reach, McpScope::Project);
        assert_eq!(legacy.effective_scope(), McpScope::Project);

        let legacy_project: McpGrant = serde_json::from_str("\"project\"").unwrap();
        assert_eq!(legacy_project.role, McpRole::Write);
        assert_eq!(legacy_project.reach, McpScope::Project);

        // New object form carries role + reach; legacy role names still decode.
        let read: McpGrant =
            serde_json::from_str(r#"{"role":"member","reach":"project"}"#).unwrap();
        assert_eq!(read.role, McpRole::Read);
        assert_eq!(read.reach, McpScope::Project);

        // Old Off grants migrate back to read, not a hidden disabled state.
        let off: McpGrant = serde_json::from_str(r#"{"role":"off","reach":"global"}"#).unwrap();
        assert_eq!(off.role, McpRole::Read);
        assert_eq!(off.effective_scope(), McpScope::Project);

        // Missing fields fall back to the default grant.
        let bare: McpGrant = serde_json::from_str("{}").unwrap();
        assert_eq!(bare, McpGrant::default());
        assert_eq!(bare.role, McpRole::Read);
        assert_eq!(bare.reach, McpScope::Project);
    }

    #[test]
    fn mcp_grant_serializes_to_object_form() {
        let grant = McpGrant {
            role: McpRole::Write,
            reach: McpScope::Project,
        };
        let json = serde_json::to_string(&grant).unwrap();
        assert_eq!(json, r#"{"role":"write","reach":"project"}"#);
        // Round trips.
        let back: McpGrant = serde_json::from_str(&json).unwrap();
        assert_eq!(back, grant);
    }

    #[test]
    fn browser_screenshot_gallery_preference_defaults_on() {
        let absent: AppState = serde_json::from_str("{}").unwrap();
        assert!(absent.mcp_auto_add_browser_screenshots);

        let disabled: AppState =
            serde_json::from_str(r#"{"mcp_auto_add_browser_screenshots":false}"#).unwrap();
        assert!(!disabled.mcp_auto_add_browser_screenshots);
    }

    #[test]
    fn session_title_mode_defaults_to_agent_and_preserves_explicit_modes() {
        let absent: AppState = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.session_title_mode, SessionTitleMode::Agent);

        let first_prompt: AppState =
            serde_json::from_str(r#"{"session_title_mode":"first_prompt"}"#).unwrap();
        assert_eq!(
            first_prompt.session_title_mode,
            SessionTitleMode::FirstPrompt
        );

        let unknown: AppState =
            serde_json::from_str(r#"{"session_title_mode":"future_mode"}"#).unwrap();
        assert_eq!(unknown.session_title_mode, SessionTitleMode::Agent);
    }

    #[test]
    fn builtin_presets_are_generated_from_runtime_descriptors() {
        let presets = builtin_global_presets();
        let mut runtimes = crate::runtime_catalog::builtin_runtime_catalog()
            .current_platform_descriptors()
            .collect::<Vec<_>>();
        runtimes.sort_by(|left, right| {
            left.legacy_order
                .unwrap_or(u16::MAX)
                .cmp(&right.legacy_order.unwrap_or(u16::MAX))
                .then_with(|| left.slug.cmp(&right.slug))
                .then_with(|| left.id.cmp(&right.id))
        });
        let declared = runtimes
            .into_iter()
            .flat_map(|runtime| runtime.suggested_presets.iter())
            .collect::<Vec<_>>();

        assert_eq!(presets.len(), declared.len());
        for (preset, descriptor) in presets.iter().zip(declared) {
            assert_eq!(preset.id, descriptor.id);
            assert_eq!(preset.label, descriptor.label);
            assert_eq!(preset.command, descriptor.command);
            assert_eq!(preset.quick_launch, descriptor.quick_launch);
        }
    }

    #[test]
    fn normalizes_pins_per_project_without_cross_project_deduping() {
        let mut state = AppState {
            projects: vec![
                Project {
                    id: "p1".into(),
                    name: "One".into(),
                    path: "/tmp/one".into(),
                    pinned_at: None,
                    workspace_id: "personal".into(),
                    parent_project_id: None,
                    sort_order: 0,
                    is_folder: false,
                    worktree_branch: None,
                    workspaces_enabled: false,
                },
                Project {
                    id: "p2".into(),
                    name: "Two".into(),
                    path: "/tmp/two".into(),
                    pinned_at: None,
                    workspace_id: "personal".into(),
                    parent_project_id: None,
                    sort_order: 1,
                    is_folder: false,
                    worktree_branch: None,
                    workspaces_enabled: false,
                },
            ],
            pinned_sessions: HashMap::from([
                (
                    "p1".into(),
                    vec![
                        PinnedSidebarSession {
                            key: "session:s1".into(),
                            project_id: "wrong".into(),
                            session_id: Some(" s1 ".into()),
                            pinned_at: 1,
                        },
                        PinnedSidebarSession {
                            key: "session:s1".into(),
                            project_id: "p1".into(),
                            session_id: Some("s1".into()),
                            pinned_at: 2,
                        },
                    ],
                ),
                (
                    "p2".into(),
                    vec![PinnedSidebarSession {
                        key: "session:s2".into(),
                        project_id: "p2".into(),
                        session_id: Some("s2".into()),
                        pinned_at: 3,
                    }],
                ),
            ]),
            ..Default::default()
        };

        assert!(normalize_pinned_sessions(&mut state));
        assert_eq!(state.pinned_sessions["p1"].len(), 1);
        assert_eq!(state.pinned_sessions["p1"][0].project_id, "p1");
        assert_eq!(
            state.pinned_sessions["p1"][0].session_id.as_deref(),
            Some("s1")
        );
        assert_eq!(state.pinned_sessions["p2"].len(), 1);
        assert_eq!(state.pinned_sessions["p2"][0].key, "session:s2");
    }

    #[test]
    fn normalizes_projects_into_parent_child_display_order() {
        let mut state = AppState {
            projects: vec![
                project("child-b", Some("root"), 5),
                project("root", None, 0),
                project("child-a", Some("root"), 0),
                project("other-root", None, 2),
            ],
            ..Default::default()
        };

        assert!(normalize_projects(&mut state));

        let ordered_ids = state
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_ids,
            vec!["root", "child-a", "child-b", "other-root"]
        );
        assert_eq!(state.projects[0].sort_order, 0);
        assert_eq!(state.projects[1].sort_order, 0);
        assert_eq!(state.projects[2].sort_order, 1);
        assert_eq!(state.projects[3].sort_order, 1);
    }

    #[test]
    fn normalizes_projects_clears_invalid_parent_cycles() {
        let mut state = AppState {
            projects: vec![
                project("alpha", Some("beta"), 0),
                project("beta", Some("alpha"), 0),
                project("orphan", Some("missing"), 0),
            ],
            ..Default::default()
        };

        assert!(normalize_projects(&mut state));

        assert_eq!(state.projects[0].id, "alpha");
        assert_eq!(state.projects[1].id, "beta");
        assert_eq!(state.projects[2].id, "orphan");
        assert!(state
            .projects
            .iter()
            .all(|project| project.parent_project_id.is_none()));
        assert_eq!(
            state
                .projects
                .iter()
                .map(|project| project.sort_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }
}
