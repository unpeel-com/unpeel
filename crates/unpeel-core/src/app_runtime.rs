//! Runtime detection for installed Unpeel Apps. App identities come from the
//! central App CLI registry when their binary exists on resolved PATH. The
//! Host recognizes one in a foreground job the same way it recognizes a
//! built-in agent runtime.
//!
//! Hard rules, mirrored from the plan:
//!
//! - **Built-ins are reserved.** An App alias that collides with any built-in
//!   runtime's command or process alias is dropped; the built-in always wins.
//!   Shells, interpreters, and wrapper names are refused outright so one App
//!   catalog entry can never claim every script on the machine.
//! - **Identity only, never Busy authority.** A matched App names/tints the
//!   session row (`kind: app`); busy/idle/attention still comes only from the
//!   App's own lifecycle reporting (the App's status reporter posting
//!   to the hook port), exactly like hook-capable agents.
//! - **Data-only.** Nothing here installs hooks, rewrites launches, or runs
//!   App code; matching an executable name to the catalog is the entire
//!   effect.
//!
//! The installed set rechecks resolved PATH like every `apps` MCP action, but
//! behind a short-lived cache because the runtime observer polls at scan
//! cadence across every live session.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Foreground observation runs ~3x/second per live session; re-resolving the
/// Host's PATH that often is pure overhead. A freshly installed App is
/// recognized on the next refresh tick instead.
const CACHE_TTL: Duration = Duration::from_secs(2);

/// Names an App catalog entry may never claim as a detection alias: claiming a
/// shell or interpreter would grab every script the user runs. Kept in sync
/// with the wrapper names the runtime observer itself parses through.
const RESERVED_EXECUTABLE_NAMES: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "login",
    "script",
    "tmux",
    "screen",
    "env",
    "command",
    "sudo",
    "ssh",
    "node",
    "bun",
    "deno",
    "npx",
    "bunx",
    "npm",
    "yarn",
    "pnpm",
    "uv",
    "uvx",
    "pip",
    "pipx",
    "git",
    "make",
    "cargo",
    "unpeel",
    "unpeel-host",
    "unpeel-attach",
];

/// One installed App projected into the runtime-detection layer. The id is
/// interned so observer matches can share the `&'static str` shape built-in
/// catalog matches use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRuntimeIdentity {
    pub app_id: &'static str,
    pub name: String,
    /// `#RRGGBB`, validated at parse time.
    pub tint: Option<String>,
    pub spinner_tint: Option<String>,
}

struct CachedIndex {
    loaded_at: Instant,
    /// normalized executable basename → identity
    by_alias: BTreeMap<String, AppRuntimeIdentity>,
    /// app id → identity (catalog/display resolution)
    by_id: BTreeMap<String, AppRuntimeIdentity>,
}

fn cache() -> &'static Mutex<Option<CachedIndex>> {
    static CACHE: OnceLock<Mutex<Option<CachedIndex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Interned App ids live for the process; the pool is bounded by the
/// installed-app cap times id length, and re-interning is free.
fn intern_app_id(id: &str) -> &'static str {
    static POOL: OnceLock<Mutex<std::collections::BTreeSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
    let mut pool = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = pool.get(id) {
        return existing;
    }
    let leaked: &'static str = Box::leak(id.to_string().into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// A declared alias must look like a plain executable basename. Anything
/// path-like, spaced, or empty is author error and is dropped, not repaired.
pub(crate) fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 64
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'/' | b'\\'))
}

/// Reserved if a built-in runtime declares it on any platform, or it is a
/// shell/interpreter/wrapper name.
pub(crate) fn alias_reserved(alias: &str) -> bool {
    if RESERVED_EXECUTABLE_NAMES.contains(&alias) {
        return true;
    }
    let catalog = crate::runtime_catalog::builtin_runtime_catalog();
    catalog.by_command_alias(alias).is_some() || catalog.by_process_alias(alias).is_some()
}

fn build_index() -> CachedIndex {
    build_index_from(&crate::apps_mcp::installed_apps())
}

fn build_index_from(apps: &[crate::apps_mcp::InstalledApp]) -> CachedIndex {
    let mut by_alias: BTreeMap<String, AppRuntimeIdentity> = BTreeMap::new();
    let mut by_id: BTreeMap<String, AppRuntimeIdentity> = BTreeMap::new();
    for app in apps {
        let identity = AppRuntimeIdentity {
            app_id: intern_app_id(&app.id),
            name: app.name.clone(),
            tint: app.tint.clone(),
            spinner_tint: app.spinner_tint.clone(),
        };
        by_id.insert(app.id.clone(), identity.clone());
        // Catalog aliases plus the implicit basename of the launch command.
        let mut aliases = app.detection_aliases.clone();
        if let Some(command) = app.command.as_deref() {
            if let Some(first) = command.split_whitespace().next() {
                aliases.push(crate::runtime_observer::normalized_executable_name(first));
            }
        }
        for alias in aliases {
            if !valid_alias(&alias) || alias_reserved(&alias) {
                continue;
            }
            // First catalog entry wins on a cross-App collision;
            // installed_apps() is id-sorted so the outcome is deterministic.
            by_alias.entry(alias).or_insert_with(|| identity.clone());
        }
    }
    CachedIndex {
        loaded_at: Instant::now(),
        by_alias,
        by_id,
    }
}

fn with_index<T>(read: impl FnOnce(&CachedIndex) -> T) -> T {
    let mut guard = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stale = guard
        .as_ref()
        .is_none_or(|cached| cached.loaded_at.elapsed() > CACHE_TTL);
    if stale {
        *guard = Some(build_index());
    }
    read(guard.as_ref().expect("index just built"))
}

/// Drop the cached index so the next lookup rechecks PATH immediately.
/// Install/remove flows and tests use this; correctness never depends on it
/// (the TTL refresh covers a missed call).
pub fn invalidate_cache() {
    *cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Match a foreground executable name (already or not yet normalized) to an
/// installed App. Built-in runtimes are consulted first by the caller; this
/// layer never shadows them because reserved aliases were dropped at build.
pub fn app_for_executable(executable: &str) -> Option<AppRuntimeIdentity> {
    let normalized = crate::runtime_observer::normalized_executable_name(executable);
    if normalized.is_empty() {
        return None;
    }
    with_index(|index| index.by_alias.get(&normalized).cloned())
}

/// Resolve a session launch command to an installed App by its first token's
/// basename, so App-launched sessions carry identity from spawn instead of
/// waiting for the first observation tick.
pub fn app_for_launch_command(command: &str) -> Option<AppRuntimeIdentity> {
    app_for_executable(command.split_whitespace().next()?)
}

/// Resolve an observed runtime id back to an installed App: app ids are only
/// ever produced by this layer, so a miss means "not an App" (a built-in id
/// or an uninstalled leftover).
pub fn app_for_runtime_id(runtime_id: &str) -> Option<AppRuntimeIdentity> {
    with_index(|index| index.by_id.get(runtime_id).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_validation_refuses_paths_and_whitespace() {
        assert!(valid_alias("unpeel-design"));
        assert!(valid_alias("todo_v2"));
        assert!(!valid_alias(""));
        assert!(!valid_alias("bin/design"));
        assert!(!valid_alias("two words"));
        assert!(!valid_alias(&"x".repeat(65)));
    }

    #[test]
    fn index_maps_aliases_and_ids_with_builtins_reserved() {
        let app = crate::apps_mcp::InstalledApp {
            id: "unpeel.app.design".into(),
            name: "Unpeel Design".into(),
            version: Some("0.1.0".into()),
            description: String::new(),
            command: Some("/opt/bin/unpeel-design --serve".into()),
            media_types: Vec::new(),
            tools: Vec::new(),
            skill_file: None,
            dir: std::path::PathBuf::from("/tmp"),
            // "claude" collides with a built-in and must be dropped; "bash"
            // is a reserved wrapper name.
            detection_aliases: vec!["design".into(), "claude".into(), "bash".into()],
            tint: Some("#8B5CF6".into()),
            spinner_tint: None,
        };
        let index = build_index_from(&[app]);
        // Declared alias + implicit command basename resolve; reserved names
        // never do.
        for alias in ["design", "unpeel-design"] {
            let identity = index.by_alias.get(alias).expect(alias);
            assert_eq!(identity.app_id, "unpeel.app.design");
            assert_eq!(identity.tint.as_deref(), Some("#8B5CF6"));
        }
        assert!(!index.by_alias.contains_key("claude"));
        assert!(!index.by_alias.contains_key("bash"));
        assert!(index.by_id.contains_key("unpeel.app.design"));
    }

    #[test]
    fn builtin_and_wrapper_names_are_reserved() {
        // Wrapper/shell names are refused outright.
        assert!(alias_reserved("bash"));
        assert!(alias_reserved("node"));
        // Built-in runtime aliases are reserved on every platform.
        assert!(alias_reserved("claude"));
        assert!(alias_reserved("codex"));
        // An ordinary novel name is free.
        assert!(!alias_reserved("unpeel-design"));
    }
}
