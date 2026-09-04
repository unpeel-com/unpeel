//! Host-owned, Host-installed Browser MCP engine.
//!
//! The Browser MCP drives the `agent-browser` engine (Apache-2.0, a native
//! Rust CDP daemon — never Node). Until 2026-09-03 the only deterministic copy
//! was the one the Mac app build bundled next to `unpeel-host`, so a headless
//! Host had no engine and the public server repo would have depended on the
//! app for it. Now the pin lives in `protocol/browser-engine-v1.json`
//! (embedded here), and the Host installs the platform binary itself into
//! `~/.unpeel/browser/bin/agent-browser` after verifying its sha256 against
//! that manifest:
//!
//! - `pinned()` — the embedded manifest.
//! - `ensure_installed(home)` — verify the managed copy (accept a matching
//!   hash; re-download when missing, mismatched, or from an older pin),
//!   download over the same rustls stack the update check uses, verify the
//!   hash BEFORE the rename into place, write the Apache-2.0 notice next to
//!   it, `chmod 755`, and log one `browser-engine` trace line. Concurrency
//!   safe: an exclusive flock on `browser/bin/.lock`; a second installer
//!   waits, then re-verifies and finds the first one's work.
//! - `resolve(home)` — the resolution order every consumer shares:
//!   `UNPEEL_AGENT_BROWSER_BIN` (or the older `UNPEEL_BROWSER_BIN`) →
//!   the verified managed copy → next to the running executable (the app
//!   bundle, kept as a compatibility candidate until the repo split) →
//!   `PATH`.
//! - `system_browser()` — the engine drives a system Chrome/Chromium; on a
//!   Host without one, `unpeel browser install --check` and the MCP error
//!   must say so and name what was looked for. Nothing here installs Chrome.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `protocol/browser-engine-v1.json`, embedded so the Host never needs the
/// repo at runtime and the Mac app build reads the same darwin hashes.
const MANIFEST_JSON: &str = include_str!("../../../protocol/browser-engine-v1.json");

/// Engine binaries are ~12–14 MiB; refuse anything absurd.
const MAX_ENGINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_NOTICE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub license: String,
    #[serde(default)]
    pub notice: Option<Source>,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Source {
    /// `darwin-arm64` | `darwin-x64` | `linux-x64` | `linux-arm64` (absent on
    /// the notice entry).
    #[serde(default)]
    pub platform: String,
    pub url: String,
    pub sha256: String,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Self, String> {
        let manifest: Manifest =
            serde_json::from_str(json).map_err(|e| format!("browser-engine manifest: {e}"))?;
        if manifest.version.trim().is_empty() {
            return Err("browser-engine manifest: empty version".into());
        }
        for source in &manifest.sources {
            if source.sha256.len() != 64 || !source.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "browser-engine manifest: {} has a malformed sha256",
                    source.platform
                ));
            }
            if !source.url.starts_with("https://") {
                return Err(format!(
                    "browser-engine manifest: {} source is not https",
                    source.platform
                ));
            }
        }
        Ok(manifest)
    }

    pub fn source_for(&self, platform: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.platform == platform)
    }
}

/// The pinned engine manifest embedded in this build.
pub fn pinned() -> Manifest {
    Manifest::parse(MANIFEST_JSON).expect("embedded browser-engine manifest is valid")
}

/// This build's manifest platform key, or `None` on a platform the pin does
/// not cover.
pub fn current_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("linux", "x86_64") => Some("linux-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        _ => None,
    }
}

pub fn install_dir(home: &Path) -> PathBuf {
    home.join("browser").join("bin")
}

pub fn binary_path(home: &Path) -> PathBuf {
    install_dir(home).join("agent-browser")
}

pub fn notice_path(home: &Path) -> PathBuf {
    install_dir(home).join("LICENSE-agent-browser.txt")
}

fn version_marker_path(home: &Path) -> PathBuf {
    install_dir(home).join("agent-browser.version")
}

/// Published additively as `serve.json.browserEngine` and printed by
/// `unpeel browser install`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `ready` | `installing` | `failed` | `missing` | `disabled`.
    pub state: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Status {
    pub fn installing() -> Self {
        Self {
            state: "installing".into(),
            version: pinned().version,
            path: None,
            error: None,
        }
    }
    pub fn ready(path: PathBuf) -> Self {
        Self {
            state: "ready".into(),
            version: pinned().version,
            path: Some(path),
            error: None,
        }
    }
    /// `UNPEEL_BROWSER_ENGINE_INSTALL=0`: the worker starts no install
    /// thread (benchmarks, air-gapped Hosts, operators who manage the engine
    /// themselves); resolution still finds an existing engine.
    pub fn disabled() -> Self {
        Self {
            state: "disabled".into(),
            version: pinned().version,
            path: None,
            error: None,
        }
    }
    pub fn failed(error: String) -> Self {
        Self {
            state: "failed".into(),
            version: pinned().version,
            path: None,
            error: Some(error),
        }
    }
}

/// Why the managed copy is not acceptable as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    Missing,
    /// Hash differs from the pin (an older or tampered engine); carries the
    /// version marker the previous install wrote, when any.
    Mismatch {
        installed_version: Option<String>,
    },
    Unsupported,
    Io(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Missing => write!(f, "not installed"),
            VerifyError::Mismatch { installed_version } => match installed_version {
                Some(v) => write!(f, "installed engine ({v}) does not match the pinned hash"),
                None => write!(f, "installed engine does not match the pinned hash"),
            },
            VerifyError::Unsupported => write!(
                f,
                "no pinned engine for {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            VerifyError::Io(e) => write!(f, "{e}"),
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String, VerifyError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(sha256_hex(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(VerifyError::Missing),
        Err(e) => Err(VerifyError::Io(format!("read {}: {e}", path.display()))),
    }
}

/// Verify the managed copy under `home` against `manifest` for `platform`.
/// Returns the binary path when the hash matches.
pub fn verify_with(
    home: &Path,
    manifest: &Manifest,
    platform: &str,
) -> Result<PathBuf, VerifyError> {
    let source = manifest
        .source_for(platform)
        .ok_or(VerifyError::Unsupported)?;
    let path = binary_path(home);
    let actual = sha256_file(&path)?;
    if actual != source.sha256 {
        let installed_version = std::fs::read_to_string(version_marker_path(home))
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        return Err(VerifyError::Mismatch { installed_version });
    }
    Ok(path)
}

/// `verify_with` against the embedded pin for this platform.
pub fn verify(home: &Path) -> Result<PathBuf, VerifyError> {
    let platform = current_platform().ok_or(VerifyError::Unsupported)?;
    verify_with(home, &pinned(), platform)
}

/// Exclusive advisory lock on `<install dir>/.lock`, held while installing.
/// Blocking: a concurrent installer waits, then re-verifies (usually finding
/// the first one's work and doing nothing).
pub struct InstallLock(#[allow(dead_code)] std::fs::File);

pub fn lock(home: &Path) -> Result<InstallLock, String> {
    use std::os::fd::AsRawFd;
    let dir = install_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let lock_path = dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "lock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(InstallLock(file))
}

/// Non-blocking variant for tests and status probes.
pub fn try_lock(home: &Path) -> Result<Option<InstallLock>, String> {
    use std::os::fd::AsRawFd;
    let dir = install_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let lock_path = dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        return if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(format!("lock {}: {err}", lock_path.display()))
        };
    }
    Ok(Some(InstallLock(file)))
}

/// Downloads a manifest source straight into `path` (never a whole-body
/// buffer) and returns its sha256 hex; production uses
/// `http_fetch::get_to_file`, tests substitute a closure.
pub type Fetch<'a> = dyn Fn(&str, &Path, usize) -> Result<String, String> + 'a;

/// Install (or confirm) the pinned engine under `home` for `platform`, using
/// `fetch` for downloads. Returns the verified binary path. Holds the install
/// lock for the whole operation.
pub fn ensure_installed_with(
    home: &Path,
    manifest: &Manifest,
    platform: &str,
    fetch: &Fetch<'_>,
) -> Result<PathBuf, String> {
    let _lock = lock(home)?;
    match verify_with(home, manifest, platform) {
        Ok(path) => {
            ensure_notice(home, manifest, fetch);
            return Ok(path);
        }
        Err(VerifyError::Unsupported) => {
            return Err(format!(
                "{}: agent-browser {} publishes no build for {}-{}",
                VerifyError::Unsupported,
                manifest.version,
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        }
        Err(VerifyError::Io(e)) => return Err(e),
        Err(reason @ (VerifyError::Missing | VerifyError::Mismatch { .. })) => {
            trace(
                home,
                &format!("installing agent-browser {} ({reason})", manifest.version),
            );
        }
    }
    let source = manifest
        .source_for(platform)
        .ok_or_else(|| VerifyError::Unsupported.to_string())?;
    let dir = install_dir(home);
    let final_path = binary_path(home);
    // Stream to a .part in the install dir (same filesystem as the final
    // path, so the rename is atomic); the hash is computed while streaming.
    let tmp = dir.join(format!(".agent-browser.{}.part", std::process::id()));
    let actual = fetch(&source.url, &tmp, MAX_ENGINE_BYTES).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("download {}: {e}", source.url)
    })?;
    if actual != source.sha256 {
        let _ = std::fs::remove_file(&tmp);
        let message = format!(
            "downloaded agent-browser {} for {platform} does not match the pinned sha256 \
(expected {}, got {actual}); nothing was installed",
            manifest.version, source.sha256
        );
        trace(home, &message);
        return Err(message);
    }
    mark_executable(&tmp).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    std::fs::rename(&tmp, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into {}: {e}", final_path.display())
    })?;
    let _ = std::fs::write(version_marker_path(home), format!("{}\n", manifest.version));
    ensure_notice(home, manifest, fetch);
    // The download went through a 64 KiB buffer, but the TLS session and
    // hasher still leave freed heap behind; hand it back so a worker that
    // installed at start does not carry the pages in its footprint.
    crate::terminal_viewport::release_memory_to_os();
    trace(
        home,
        &format!(
            "installed agent-browser {} for {platform} at {} (sha256 {})",
            manifest.version,
            final_path.display(),
            &actual[..12]
        ),
    );
    Ok(final_path)
}

/// `ensure_installed_with` against the embedded pin, this platform, and the
/// rustls HTTP helper.
pub fn ensure_installed(home: &Path) -> Result<PathBuf, String> {
    let platform = current_platform().ok_or_else(|| {
        format!(
            "no pinned browser engine for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    ensure_installed_with(home, &pinned(), platform, &|url, path, max| {
        crate::http_fetch::get_to_file(url, path, max).map(|(_, sha)| sha)
    })
}

fn mark_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

/// The Apache-2.0 notice next to the binary. Best effort: a missing notice
/// never fails an install (the trace line records it); a notice whose hash
/// is pinned is verified like the binary.
fn ensure_notice(home: &Path, manifest: &Manifest, fetch: &Fetch<'_>) {
    let path = notice_path(home);
    let Some(notice) = manifest.notice.as_ref() else {
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!(
                    "agent-browser {} is licensed under {}.\n",
                    manifest.version, manifest.license
                ),
            );
        }
        return;
    };
    if let Ok(existing) = std::fs::read(&path) {
        if sha256_hex(&existing) == notice.sha256 {
            return;
        }
    }
    let tmp = path.with_extension("part");
    match fetch(&notice.url, &tmp, MAX_NOTICE_BYTES) {
        Ok(sha) if sha == notice.sha256 => {
            let _ = std::fs::rename(&tmp, &path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp);
            trace(
                home,
                "license notice download did not match its pinned sha256; skipped",
            );
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            trace(home, &format!("license notice download failed: {e}"));
        }
    }
}

/// Resolution order shared by the MCP server, the CLI verb, and the worker.
/// Every candidate must be an existing file; the managed copy must also pass
/// hash verification (a stale managed engine is skipped, not used).
pub fn resolve_with(
    env_override: Option<&str>,
    home: &Path,
    exe_dir: Option<&Path>,
    path_dirs: &[PathBuf],
) -> Result<PathBuf, String> {
    if let Some(value) = env_override {
        let path = PathBuf::from(value.trim());
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Ok(path) = verify(home) {
        return Ok(path);
    }
    if let Some(dir) = exe_dir {
        let bundled = dir.join("agent-browser");
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    for dir in path_dirs {
        let candidate = dir.join("agent-browser");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(missing_engine_message(home))
}

/// `resolve_with` from the process environment.
pub fn resolve(home: &Path) -> Result<PathBuf, String> {
    let env_override = std::env::var("UNPEEL_AGENT_BROWSER_BIN")
        .ok()
        .or_else(|| std::env::var("UNPEEL_BROWSER_BIN").ok())
        .filter(|v| !v.trim().is_empty());
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    resolve_with(
        env_override.as_deref(),
        home,
        exe_dir.as_deref(),
        &crate::setup::search_dirs(),
    )
}

pub fn missing_engine_message(home: &Path) -> String {
    let managed = binary_path(home);
    let hint = match verify(home) {
        Err(VerifyError::Mismatch { installed_version }) => format!(
            "{} exists but does not match the pinned agent-browser {}{}",
            managed.display(),
            pinned().version,
            installed_version
                .map(|v| format!(" (installed: {v})"))
                .unwrap_or_default()
        ),
        Err(VerifyError::Unsupported) => VerifyError::Unsupported.to_string(),
        _ => format!("{} is not installed", managed.display()),
    };
    format!(
        "The browser engine (agent-browser {}) is not available: {hint}. Run `unpeel browser \
install` on this Host (or set UNPEEL_AGENT_BROWSER_BIN to an engine binary).",
        pinned().version
    )
}

/// Where a system Chrome/Chromium was looked for, in order — for the
/// explicit "no browser on this Host" message.
pub fn system_browser_candidates(path_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if std::env::consts::OS == "macos" {
        for app in [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ] {
            out.push(PathBuf::from(app));
        }
        if let Some(home) = dirs::home_dir() {
            out.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
        }
    }
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "brave-browser",
        "microsoft-edge",
    ] {
        for dir in path_dirs {
            out.push(dir.join(name));
        }
    }
    out
}

/// First existing system browser, if any.
pub fn system_browser(path_dirs: &[PathBuf]) -> Option<PathBuf> {
    system_browser_candidates(path_dirs)
        .into_iter()
        .find(|p| p.is_file())
}

/// One line for the "no browser" case, naming what was looked for.
pub fn missing_browser_message(path_dirs: &[PathBuf]) -> String {
    let names: Vec<String> = system_browser_candidates(path_dirs)
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .fold(Vec::new(), |mut acc, n| {
            if !acc.contains(&n) {
                acc.push(n);
            }
            acc
        });
    format!(
        "no Chrome/Chromium found on this Host (looked for {} in /Applications and on PATH). The \
engine drives a system browser; install Google Chrome or Chromium, or run `agent-browser install` \
to fetch Chrome for Testing into the engine's own cache. Unpeel does not install a browser.",
        names.join(", ")
    )
}

fn trace(home: &Path, message: &str) {
    let path = home.join("hooks").join("trace.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "{} browser-engine {message}",
            crate::state::current_timestamp_ms()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unpeel-browser-engine-{tag}-{}-{}",
            std::process::id(),
            crate::state::current_timestamp_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest_for(bytes: &[u8]) -> Manifest {
        Manifest {
            version: "9.9.9".into(),
            license: "Apache-2.0".into(),
            notice: None,
            sources: vec![Source {
                platform: "test-plat".into(),
                url: "https://example.invalid/agent-browser".into(),
                sha256: sha256_hex(bytes),
            }],
        }
    }

    #[test]
    fn embedded_manifest_parses_and_covers_every_platform() {
        let manifest = pinned();
        assert_eq!(manifest.version, "0.34.0");
        assert_eq!(manifest.license, "Apache-2.0");
        for platform in ["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"] {
            let source = manifest.source_for(platform).expect(platform);
            assert!(source.url.ends_with(&format!("agent-browser-{platform}")));
            assert_eq!(source.sha256.len(), 64);
        }
        assert!(manifest.notice.is_some());
        assert!(current_platform()
            .map(|p| manifest.source_for(p).is_some())
            .unwrap_or(true));
    }

    #[test]
    fn malformed_manifest_is_rejected() {
        assert!(Manifest::parse("{}").is_err());
        let bad_hash = r#"{"version":"1","license":"x","sources":[{"platform":"p","url":"https://x/y","sha256":"abc"}]}"#;
        assert!(Manifest::parse(bad_hash).unwrap_err().contains("sha256"));
        let http = r#"{"version":"1","license":"x","sources":[{"platform":"p","url":"http://x/y","sha256":"0000000000000000000000000000000000000000000000000000000000000000"}]}"#;
        assert!(Manifest::parse(http).unwrap_err().contains("https"));
    }

    #[test]
    fn install_downloads_verifies_and_places_the_binary() {
        let home = temp_home("install");
        let engine = b"#!/bin/sh\necho agent-browser 9.9.9\n".to_vec();
        let manifest = manifest_for(&engine);
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&fetches);
        let fetch = move |_url: &str, path: &Path, _max: usize| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::fs::write(path, &engine).unwrap();
            Ok(sha256_hex(&engine))
        };
        let path = ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap();
        assert_eq!(path, binary_path(&home));
        assert_eq!(verify_with(&home, &manifest, "test-plat").unwrap(), path);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(notice_path(&home).exists());
        assert_eq!(
            std::fs::read_to_string(version_marker_path(&home))
                .unwrap()
                .trim(),
            "9.9.9"
        );
        // A second call re-verifies and does not download again.
        ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        let trace = std::fs::read_to_string(home.join("hooks/trace.log")).unwrap();
        assert!(trace.contains("browser-engine installed agent-browser 9.9.9"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_mismatched_download_installs_nothing() {
        let home = temp_home("reject");
        let manifest = manifest_for(b"the real engine");
        let fetch = |_url: &str, path: &Path, _max: usize| {
            std::fs::write(path, b"tampered bytes").unwrap();
            Ok(sha256_hex(b"tampered bytes"))
        };
        let err = ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap_err();
        assert!(err.contains("does not match the pinned sha256"), "{err}");
        assert!(!binary_path(&home).exists());
        assert!(std::fs::read_dir(install_dir(&home)).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_failed_download_leaves_no_part_file() {
        let home = temp_home("failed-download");
        let manifest = manifest_for(b"x");
        let fetch = |_url: &str, path: &Path, _max: usize| {
            std::fs::write(path, b"half").unwrap();
            Err("connection reset".to_string())
        };
        let err = ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap_err();
        assert!(err.contains("connection reset"), "{err}");
        assert!(!binary_path(&home).exists());
        assert!(std::fs::read_dir(install_dir(&home)).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_stale_managed_engine_is_reverified_and_replaced() {
        let home = temp_home("stale");
        std::fs::create_dir_all(install_dir(&home)).unwrap();
        std::fs::write(binary_path(&home), b"old engine").unwrap();
        std::fs::write(version_marker_path(&home), "1.0.0\n").unwrap();
        let fresh = b"new engine".to_vec();
        let manifest = manifest_for(&fresh);
        assert_eq!(
            verify_with(&home, &manifest, "test-plat").unwrap_err(),
            VerifyError::Mismatch {
                installed_version: Some("1.0.0".into())
            }
        );
        let message = {
            // resolve_with skips the stale managed copy rather than using it
            // (verify() uses the real pin, which the fake bytes also fail).
            let err = resolve_with(None, &home, None, &[]).unwrap_err();
            assert!(err.contains("unpeel browser install"), "{err}");
            err
        };
        assert!(message.contains("not available"));
        let fetch = move |_url: &str, path: &Path, _max: usize| {
            std::fs::write(path, &fresh).unwrap();
            Ok(sha256_hex(&fresh))
        };
        ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap();
        assert_eq!(std::fs::read(binary_path(&home)).unwrap(), b"new engine");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unsupported_platform_is_a_clear_error() {
        let home = temp_home("unsupported");
        let manifest = manifest_for(b"x");
        let fetch = |_url: &str, _path: &Path, _max: usize| Ok(String::new());
        let err = ensure_installed_with(&home, &manifest, "nope-plat", &fetch).unwrap_err();
        assert!(err.contains("publishes no build"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_second_installer_waits_for_the_lock_then_reverifies() {
        let home = temp_home("lock");
        let engine = b"locked engine".to_vec();
        let manifest = manifest_for(&engine);
        let held = lock(&home).unwrap();
        assert!(try_lock(&home).unwrap().is_none(), "lock must be exclusive");
        // Explicit readiness instead of timing: the worker announces that it
        // is about to take the lock, and marks completion with a flag. While
        // this thread holds the lock the flag cannot flip and nothing can be
        // installed, whatever the scheduler does under load.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let home2 = home.clone();
        let manifest2 = manifest.clone();
        let engine2 = engine.clone();
        let done2 = Arc::clone(&done);
        let worker = std::thread::spawn(move || {
            let fetch = move |_url: &str, path: &Path, _max: usize| {
                std::fs::write(path, &engine2).unwrap();
                Ok(sha256_hex(&engine2))
            };
            ready_tx.send(()).unwrap();
            let path = ensure_installed_with(&home2, &manifest2, "test-plat", &fetch).unwrap();
            done2.store(true, Ordering::SeqCst);
            path
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker started");
        assert!(
            !done.load(Ordering::SeqCst) && !binary_path(&home).exists(),
            "installer must not run while the lock is held"
        );
        drop(held);
        let path = worker.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(path, binary_path(&home));
        assert_eq!(std::fs::read(&path).unwrap(), engine);
        assert!(try_lock(&home).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolution_order_env_managed_bundle_path() {
        let home = temp_home("resolve");
        let fake = home.join("override-engine");
        std::fs::write(&fake, b"x").unwrap();
        let exe_dir = home.join("bundle");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(exe_dir.join("agent-browser"), b"bundled").unwrap();
        let path_dir = home.join("path");
        std::fs::create_dir_all(&path_dir).unwrap();
        std::fs::write(path_dir.join("agent-browser"), b"on-path").unwrap();

        // env override wins even when everything else exists
        assert_eq!(
            resolve_with(
                Some(fake.to_str().unwrap()),
                &home,
                Some(&exe_dir),
                std::slice::from_ref(&path_dir)
            )
            .unwrap(),
            fake
        );
        // a missing env override is skipped, not fatal
        assert_eq!(
            resolve_with(
                Some("/nonexistent/engine"),
                &home,
                Some(&exe_dir),
                std::slice::from_ref(&path_dir)
            )
            .unwrap(),
            exe_dir.join("agent-browser")
        );
        // managed copy only counts when it verifies against the real pin: an
        // unverified file there is skipped in favour of the bundle
        std::fs::create_dir_all(install_dir(&home)).unwrap();
        std::fs::write(binary_path(&home), b"not the pinned engine").unwrap();
        assert_eq!(
            resolve_with(None, &home, Some(&exe_dir), std::slice::from_ref(&path_dir)).unwrap(),
            exe_dir.join("agent-browser")
        );
        // no bundle → PATH
        assert_eq!(
            resolve_with(None, &home, None, std::slice::from_ref(&path_dir)).unwrap(),
            path_dir.join("agent-browser")
        );
        // nothing → the install hint
        let err = resolve_with(None, &home, None, &[]).unwrap_err();
        assert!(err.contains("unpeel browser install"));
        assert!(
            err.contains("does not match the pinned agent-browser"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn missing_browser_message_names_the_candidates() {
        let message = missing_browser_message(&[PathBuf::from("/usr/bin")]);
        assert!(message.contains("google-chrome"));
        assert!(message.contains("chromium"));
        assert!(message.contains("does not install a browser"));
    }
}
