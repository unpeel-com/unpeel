use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct DepStatus {
    pub name: String,
    pub installed: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyReport {
    pub ai_tools: Vec<DepStatus>,
    pub any_ai_installed: bool,
}

const PATH_MARKER_START: &str = "__UNPEEL_PATH_START__";
const PATH_MARKER_END: &str = "__UNPEEL_PATH_END__";

fn extract_marked<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = text.find(start)? + start.len();
    let rest = &text[start_idx..];
    let end_idx = rest.find(end)?;
    Some(&rest[..end_idx])
}

fn split_path_list(value: &str) -> Vec<PathBuf> {
    std::env::split_paths(value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

fn current_env_path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn platform_default_shell() -> &'static str {
    "/bin/zsh"
}

#[cfg(not(target_os = "macos"))]
fn platform_default_shell() -> &'static str {
    "/bin/sh"
}

/// Resolve the user's login shell without assuming macOS. Minimal Linux
/// services and containers commonly omit `SHELL`; `/bin/zsh` is not a
/// portable fallback there, while `/bin/sh` is part of the Unix baseline.
pub fn resolved_user_shell() -> String {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| is_executable_file(path))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| platform_default_shell().to_string())
}

/// The interactive login shell's PATH, memoized for the life of the process.
///
/// Reading it requires spawning `zsh -i` (so version-manager dirs like nvm/
/// pyenv that only exist in an interactive shell are picked up), which sources
/// the user's full `~/.zshrc` and costs ~1s+. The value is static for a
/// process — PATH does not change while a session host runs — so it must be
/// probed at most once here.
///
/// This memoization is load-bearing: `search_dirs()` feeds `installed_apps()`,
/// which the per-session runtime observer calls to rebuild its App-runtime
/// index on a short TTL. Without the cache, every hosted session re-spawns an
/// interactive shell every couple of seconds; with many sessions alive that
/// becomes an interactive-shell storm that saturates the machine and starves a
/// new session's first PTY output, making heavy-startup agents (codex) appear
/// to die instantly on launch. `OnceLock::get_or_init` also serializes racing
/// callers to a single probe.
/// How long a probed PATH is trusted across processes before a re-probe. The
/// login shell's PATH barely changes; a workspace-wide cache means many hosts
/// and observers share ONE probe per 10 minutes instead of each spawning
/// `zsh -i` on its own timer — the interactive-shell storm that drove the load
/// average to triple digits with leftover hosts.
const PATH_CACHE_TTL_MS: u64 = 10 * 60 * 1000;

/// A probe that came back EMPTY is trusted for only this long, not the full
/// `PATH_CACHE_TTL_MS`. An empty result is almost always transient: a shell
/// that failed to answer under load, a half-set-up profile, or a leftover host
/// with a stripped environment. Caching it for ten minutes lets one bad probe
/// poison every host in the workspace — 0.5.0 deliberately never cached an
/// empty probe at all ("must not poison the cache for the life of a
/// long-running host"). A short TTL keeps the storm throttle (a genuinely
/// empty PATH re-probes at most every few seconds, not every tick) without the
/// workspace-wide poisoning window.
const EMPTY_PATH_CACHE_TTL_MS: u64 = 5 * 1000;

fn path_cache_file() -> PathBuf {
    crate::app_paths::unpeel_home().join("path-probe-cache.json")
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// A fresh cached PATH for this workspace, or None when absent/stale/unreadable.
fn read_cached_shell_path_dirs() -> Option<Vec<PathBuf>> {
    let raw = std::fs::read(path_cache_file()).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    parse_cached_dirs(&value, now_unix_ms())
}

/// Pure cache-entry parse: `Some(dirs)` only when the entry carries a
/// `probed_at_unix_ms` within its TTL of `now_ms`; stale or malformed → None.
/// An empty entry uses the much shorter `EMPTY_PATH_CACHE_TTL_MS` so a
/// transient empty probe cannot poison the workspace for the full window.
fn parse_cached_dirs(value: &serde_json::Value, now_ms: u64) -> Option<Vec<PathBuf>> {
    let probed_at = value.get("probed_at_unix_ms").and_then(|v| v.as_u64())?;
    let dirs: Vec<PathBuf> = value
        .get("dirs")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str())
        .map(PathBuf::from)
        .collect();
    let ttl = if dirs.is_empty() {
        EMPTY_PATH_CACHE_TTL_MS
    } else {
        PATH_CACHE_TTL_MS
    };
    if now_ms.saturating_sub(probed_at) >= ttl {
        return None;
    }
    Some(dirs)
}

/// Persist the probe result for the workspace. Empty results are cached too,
/// but read back under a much shorter TTL (see `EMPTY_PATH_CACHE_TTL_MS`): the
/// point is to throttle a re-probe storm from a leftover/misconfigured host
/// without letting one transient empty probe poison the workspace for ten
/// minutes. A best-effort atomic write.
fn write_cached_shell_path_dirs(dirs: &[PathBuf]) {
    let value = serde_json::json!({
        "dirs": dirs.iter().map(|d| d.to_string_lossy()).collect::<Vec<_>>(),
        "probed_at_unix_ms": now_unix_ms(),
    });
    let Ok(body) = serde_json::to_vec(&value) else {
        return;
    };
    let path = path_cache_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn shell_path_dirs() -> Vec<PathBuf> {
    // 1. Process-local: probe at most once per process (cheapest path).
    static CACHE: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    if let Some(dirs) = CACHE.get() {
        return dirs.clone();
    }
    // 2. Workspace-wide disk cache shared across every host/observer process:
    //    a fresh entry (probed < 10 min ago) is reused without spawning a
    //    shell, so a fleet of hosts pays one probe per workspace per TTL.
    if let Some(dirs) = read_cached_shell_path_dirs() {
        if !dirs.is_empty() {
            let _ = CACHE.set(dirs.clone());
        }
        return dirs;
    }
    // 3. Miss/stale: probe once, then record it (even when empty) to throttle.
    let dirs = probe_shell_path_dirs();
    write_cached_shell_path_dirs(&dirs);
    if !dirs.is_empty() {
        let _ = CACHE.set(dirs.clone());
    }
    dirs
}

fn probe_shell_path_dirs() -> Vec<PathBuf> {
    let shell = resolved_user_shell();
    let script = format!("printf '{PATH_MARKER_START}%s{PATH_MARKER_END}' \"$PATH\"");
    let mut command = std::process::Command::new(shell);
    command.args(["-i", "-c", &script]);
    // Detach the probe from any controlling terminal it might inherit. This
    // runs `zsh -i`, which enables job control whenever it has a controlling
    // tty — and an interactive shell with job control will `tcsetpgrp` itself
    // into the foreground. When this probe is reached from a process that
    // inherited an agent's PTY (e.g. the `unpeel-host __mcp__` server a running
    // Codex spawns as a stdio child), that foreground grab steals the terminal
    // from the agent, whose next read then takes SIGTTIN and is "suspended
    // (tty input)". The probe never needs the terminal: give it no stdin and a
    // fresh session so it can never touch the agent's foreground group.
    command.stdin(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            // New session => no controlling terminal => job control no-ops and
            // tcsetpgrp has no tty to act on. Failure is non-fatal (already a
            // session leader); the probe still yields PATH.
            libc::setsid();
            Ok(())
        });
    }
    let output = command.output();

    let Ok(output) = output else {
        return Vec::new();
    };

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    extract_marked(&combined, PATH_MARKER_START, PATH_MARKER_END)
        .map(split_path_list)
        .unwrap_or_default()
}

fn common_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];

    if let Some(home) = dirs::home_dir() {
        // Runtime-owned executable locations come from the discovered
        // descriptors. The catalog validator guarantees every suffix is a
        // safe relative path beneath the user's home.
        dirs.extend(
            crate::runtime_catalog::builtin_runtime_catalog()
                .current_platform_descriptors()
                .flat_map(|runtime| runtime.detection.search_path_suffixes.iter())
                .map(|suffix| home.join(suffix)),
        );
        dirs.extend([
            home.join(".bun").join("bin"),
            home.join(".cargo").join("bin"),
            home.join(".local").join("bin"),
            home.join(".npm-global").join("bin"),
            home.join("Library").join("pnpm"),
            home.join(".local").join("share").join("pnpm"),
            home.join("bin"),
        ]);
    }

    dirs
}

/// Returns the full PATH string by merging the process PATH, interactive shell
/// PATH, and common bin directories.  This is the PATH that should be used as
/// `UNPEEL_ORIGINAL_PATH` so that wrapper scripts can locate binaries
/// installed via version managers (nvm, rbenv, pyenv, etc.) that are only
/// present in an interactive shell.
pub fn resolved_shell_path() -> String {
    let dirs = search_dirs();
    std::env::join_paths(&dirs)
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|_| {
            dirs.iter()
                .map(|d| d.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(":")
        })
}

pub fn search_dirs() -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut dirs = Vec::new();

    for dir in current_env_path_dirs()
        .into_iter()
        .chain(shell_path_dirs())
        .chain(common_bin_dirs())
    {
        if !dir.as_os_str().is_empty() && seen.insert(dir.clone()) {
            dirs.push(dir);
        }
    }

    dirs
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

pub fn find_command_path(name: &str, dirs: &[PathBuf]) -> Option<String> {
    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
        .map(|path| path.to_string_lossy().to_string())
}

fn runtime_command_names() -> Vec<&'static str> {
    let mut runtimes = crate::runtime_catalog::builtin_runtime_catalog()
        .current_platform_descriptors()
        .collect::<Vec<_>>();
    runtimes.sort_by_key(|runtime| runtime.legacy_order.unwrap_or(u16::MAX));
    runtimes
        .into_iter()
        .filter_map(|runtime| {
            runtime
                .detection
                .command_aliases
                .first()
                .map(String::as_str)
        })
        .collect()
}

pub fn dependency_report() -> DependencyReport {
    let dirs = search_dirs();
    let ai_tools: Vec<DepStatus> = runtime_command_names()
        .into_iter()
        .map(|name| {
            let path = find_command_path(name, &dirs);
            DepStatus {
                name: name.to_string(),
                installed: path.is_some(),
                path,
            }
        })
        .collect();

    let any_ai_installed = ai_tools.iter().any(|t| t.installed);

    DependencyReport {
        ai_tools,
        any_ai_installed,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        common_bin_dirs, extract_marked, find_command_path, parse_cached_dirs,
        platform_default_shell, runtime_command_names, EMPTY_PATH_CACHE_TTL_MS, PATH_CACHE_TTL_MS,
    };
    use std::path::PathBuf;

    #[test]
    fn path_cache_entry_is_reused_while_fresh_and_dropped_when_stale() {
        let now = 1_000_000_000u64;
        let fresh = serde_json::json!({
            "dirs": ["/opt/homebrew/bin", "/usr/bin"],
            "probed_at_unix_ms": now - 1_000,
        });
        assert_eq!(
            parse_cached_dirs(&fresh, now),
            Some(vec![
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/bin")
            ])
        );
        // Exactly at the TTL boundary and beyond it is stale (re-probe).
        let stale = serde_json::json!({
            "dirs": ["/usr/bin"],
            "probed_at_unix_ms": now - PATH_CACHE_TTL_MS,
        });
        assert_eq!(parse_cached_dirs(&stale, now), None);
        // A very-fresh empty entry is honored (it throttles a re-probe storm).
        let empty_fresh = serde_json::json!({ "dirs": [], "probed_at_unix_ms": now });
        assert_eq!(parse_cached_dirs(&empty_fresh, now), Some(Vec::new()));
        // But an empty entry uses the SHORT TTL: past it the probe repeats,
        // so a transient empty result cannot poison the workspace for the full
        // ten minutes (the 0.5.1 → 0.5.2 fix for the takeover-under-load storm).
        let empty_short_stale =
            serde_json::json!({ "dirs": [], "probed_at_unix_ms": now - EMPTY_PATH_CACHE_TTL_MS });
        assert_eq!(parse_cached_dirs(&empty_short_stale, now), None);
        // A non-empty entry of the same age is still fresh (long TTL).
        let full_same_age = serde_json::json!({
            "dirs": ["/usr/bin"],
            "probed_at_unix_ms": now - EMPTY_PATH_CACHE_TTL_MS,
        });
        assert_eq!(
            parse_cached_dirs(&full_same_age, now),
            Some(vec![PathBuf::from("/usr/bin")])
        );
        // Malformed entries never parse.
        assert_eq!(
            parse_cached_dirs(&serde_json::json!({ "dirs": ["/x"] }), now),
            None
        );
        assert_eq!(
            parse_cached_dirs(&serde_json::json!({ "probed_at_unix_ms": now }), now),
            None
        );
    }

    #[test]
    fn extract_marked_handles_shell_noise() {
        let text = "noise\n__UNPEEL_PATH_START__/a:/b__UNPEEL_PATH_END__\nmore";
        assert_eq!(
            extract_marked(text, "__UNPEEL_PATH_START__", "__UNPEEL_PATH_END__"),
            Some("/a:/b")
        );
    }

    #[test]
    fn common_bins_include_kimi_code_install_dir() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        for runtime in
            crate::runtime_catalog::builtin_runtime_catalog().current_platform_descriptors()
        {
            for suffix in &runtime.detection.search_path_suffixes {
                assert!(common_bin_dirs().contains(&home.join(suffix)));
            }
        }
    }

    #[test]
    fn platform_shell_fallback_matches_the_host_os() {
        #[cfg(target_os = "macos")]
        assert_eq!(platform_default_shell(), "/bin/zsh");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(platform_default_shell(), "/bin/sh");
    }

    #[test]
    fn setup_scan_discovers_every_catalog_runtime_in_legacy_order() {
        let names = runtime_command_names();
        let catalog = crate::runtime_catalog::builtin_runtime_catalog();
        let mut local = catalog.current_platform_descriptors().collect::<Vec<_>>();
        local.sort_by_key(|runtime| runtime.legacy_order.unwrap_or(u16::MAX));
        assert_eq!(names.len(), local.len());
        for (name, expected) in names.into_iter().zip(local) {
            let runtime = catalog.by_command_alias(name).expect("catalog runtime");
            assert_eq!(runtime.id, expected.id);
            assert!(runtime.supports_current_platform());
        }
    }

    #[cfg(unix)]
    #[test]
    fn find_command_path_finds_executable_file() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "unpeel-setup-test-{}-{}",
            std::process::id(),
            crate::state::current_timestamp_ms()
        ));
        fs::create_dir_all(&root).unwrap();

        let candidate = root.join("codex");
        fs::write(&candidate, "#!/bin/sh\n").unwrap();
        let mut perms = fs::metadata(&candidate).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&candidate, perms).unwrap();

        let found = find_command_path("codex", &[PathBuf::from(&root)]);
        assert_eq!(found.as_deref(), Some(candidate.to_string_lossy().as_ref()));

        let _ = fs::remove_file(candidate);
        let _ = fs::remove_dir(root);
    }
}
