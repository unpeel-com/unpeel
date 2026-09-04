//! Host-owned, Host-installed Computer Use engine.
//!
//! The `computer` MCP domain drives **cua-driver** (MIT, pure Rust; the
//! embedded daemon `computer_mcp.rs` talks to over a private socket). Until
//! 2026-09-03 the only copies were a hand-built 0.12.2 under
//! `~/.unpeel/computer/bin` and whatever the development app bundle carried,
//! so a Linux Host had no engine at all. Now the pin lives in
//! `protocol/computer-engine-v1.json` (embedded here) and the Host installs
//! the platform binary itself, exactly like `browser_engine.rs` does for
//! agent-browser, with one difference: cua publishes **tarballs**, so the
//! install verifies the archive's sha256, extracts exactly one member, and
//! verifies that member's own sha256 before the rename into place.
//!
//! - `pinned()` — the embedded manifest.
//! - `ensure_installed(home)` — verify the managed copy (accept a matching
//!   hash; re-download when missing, mismatched, or from an older pin),
//!   stream the archive to a `.part` next to the final path (never a
//!   whole-body buffer), verify it, extract the one member into a private
//!   directory, verify the member, `chmod 755`, rename into place, write the
//!   MIT notice next to it, and log one `computer-engine` trace line.
//!   Concurrency safe: an exclusive flock on `computer/bin/.lock`; a second
//!   installer waits, then re-verifies and finds the first one's work.
//! - `resolve(home)` — the resolution order every consumer shares:
//!   `UNPEEL_CUA_DRIVER_BIN` → the verified managed copy → next to the
//!   running executable (the development app bundle) → `PATH`. A stale
//!   managed copy is skipped, not used.
//! - `graphical_session()` — the Linux desktop-session check `unpeel serve`
//!   and `unpeel computer install --check` share. Lane B extends it with
//!   the session bus; nothing here starts a daemon.
//! - `install_enabled()` — `UNPEEL_COMPUTER_ENGINE_INSTALL=0` keeps the
//!   worker from installing (benchmarks, process tests, air-gapped Hosts).
//!
//! Extraction shells out to the system `tar` (present on every macOS and
//! Linux Host and in the release CI image) with a fixed member name, after
//! the archive hash has already been verified — no archive-format crate,
//! no dependency churn in the NOTICE snapshot.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// `protocol/computer-engine-v1.json`, embedded so the Host never needs the
/// repo at runtime and the development app build reads the same darwin
/// hashes.
const MANIFEST_JSON: &str = include_str!("../../../protocol/computer-engine-v1.json");

/// Release archives are 29–42 MiB; refuse anything absurd.
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_NOTICE_BYTES: usize = 256 * 1024;

pub const ENV_OVERRIDE: &str = "UNPEEL_CUA_DRIVER_BIN";
pub const ENV_INSTALL: &str = "UNPEEL_COMPUTER_ENGINE_INSTALL";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: String,
    pub license: String,
    #[serde(default)]
    pub notice: Option<Source>,
    /// The one file extracted from every archive (`cua-driver`).
    pub archive_member: String,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// `darwin-arm64` | `darwin-x64` | `linux-x64` | `linux-arm64` (absent on
    /// the notice entry).
    #[serde(default)]
    pub platform: String,
    pub url: String,
    /// sha256 of the download itself (the tarball, or the notice text).
    pub sha256: String,
    /// sha256 of the extracted `archive_member` — what the managed copy on
    /// disk is verified against (absent on the notice entry).
    #[serde(default)]
    pub binary_sha256: String,
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Self, String> {
        let manifest: Manifest =
            serde_json::from_str(json).map_err(|e| format!("computer-engine manifest: {e}"))?;
        if manifest.version.trim().is_empty() {
            return Err("computer-engine manifest: empty version".into());
        }
        let member = manifest.archive_member.as_str();
        if member.is_empty() || member.contains('/') || member.contains('\\') || member == ".." {
            return Err("computer-engine manifest: archiveMember must be a bare file name".into());
        }
        if let Some(notice) = &manifest.notice {
            if !is_sha256_hex(&notice.sha256) || !notice.url.starts_with("https://") {
                return Err("computer-engine manifest: malformed notice entry".into());
            }
        }
        for source in &manifest.sources {
            if !is_sha256_hex(&source.sha256) {
                return Err(format!(
                    "computer-engine manifest: {} has a malformed sha256",
                    source.platform
                ));
            }
            if !is_sha256_hex(&source.binary_sha256) {
                return Err(format!(
                    "computer-engine manifest: {} has a malformed binarySha256",
                    source.platform
                ));
            }
            if !source.url.starts_with("https://") {
                return Err(format!(
                    "computer-engine manifest: {} source is not https",
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
    Manifest::parse(MANIFEST_JSON).expect("embedded computer-engine manifest is valid")
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
    home.join("computer").join("bin")
}

pub fn binary_path(home: &Path) -> PathBuf {
    install_dir(home).join("cua-driver")
}

pub fn notice_path(home: &Path) -> PathBuf {
    install_dir(home).join("LICENSE-cua-driver.txt")
}

fn version_marker_path(home: &Path) -> PathBuf {
    install_dir(home).join("cua-driver.version")
}

/// Published additively as `serve.json.computerEngine` and printed by
/// `unpeel computer install`.
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
    /// Not installed and not (yet) requested: the worker installs on demand
    /// once Computer Use is turned on, `unpeel computer install` at any time.
    pub fn missing(error: Option<String>) -> Self {
        Self {
            state: "missing".into(),
            version: pinned().version,
            path: None,
            error,
        }
    }
    /// `UNPEEL_COMPUTER_ENGINE_INSTALL=0`: the worker starts no install
    /// thread (benchmarks, process tests, air-gapped Hosts, operators who
    /// manage the engine themselves); resolution still finds an existing
    /// engine.
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

/// `UNPEEL_COMPUTER_ENGINE_INSTALL=0` (or `false`/`off`/`no`) opts the
/// worker out of installing the engine.
pub fn install_enabled() -> bool {
    install_enabled_from(std::env::var(ENV_INSTALL).ok().as_deref())
}

pub fn install_enabled_from(value: Option<&str>) -> bool {
    !matches!(
        value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
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

pub use crate::browser_engine::sha256_hex;

/// sha256 of a file through a 64 KiB buffer (the engine is 45–62 MiB; never
/// read it whole).
fn sha256_file(path: &Path) -> Result<String, VerifyError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(VerifyError::Missing),
        Err(e) => return Err(VerifyError::Io(format!("open {}: {e}", path.display()))),
    };
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| VerifyError::Io(format!("read {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Identity of a verified managed copy: hashing 60 MiB per MCP call would be
/// wasteful, so a process remembers the (path, len, mtime) it last verified
/// and only re-hashes when the file changed underneath it.
static VERIFIED: Mutex<Option<(PathBuf, u64, SystemTime, String)>> = Mutex::new(None);

fn file_identity(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
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
    let identity = file_identity(&path);
    if let (Some((len, mtime)), Ok(guard)) = (identity, VERIFIED.lock()) {
        if let Some((cached_path, cached_len, cached_mtime, cached_sha)) = guard.as_ref() {
            if cached_path == &path
                && *cached_len == len
                && *cached_mtime == mtime
                && *cached_sha == source.binary_sha256
            {
                return Ok(path);
            }
        }
    }
    let actual = sha256_file(&path)?;
    if actual != source.binary_sha256 {
        let installed_version = std::fs::read_to_string(version_marker_path(home))
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        return Err(VerifyError::Mismatch { installed_version });
    }
    if let (Some((len, mtime)), Ok(mut guard)) = (identity, VERIFIED.lock()) {
        *guard = Some((path.clone(), len, mtime, actual));
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

fn open_lock_file(home: &Path) -> Result<std::fs::File, String> {
    let dir = install_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let lock_path = dir.join(".lock");
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open {}: {e}", lock_path.display()))
}

pub fn lock(home: &Path) -> Result<InstallLock, String> {
    use std::os::fd::AsRawFd;
    let file = open_lock_file(home)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "lock {}: {}",
            install_dir(home).join(".lock").display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(InstallLock(file))
}

/// Non-blocking variant for tests and status probes.
pub fn try_lock(home: &Path) -> Result<Option<InstallLock>, String> {
    use std::os::fd::AsRawFd;
    let file = open_lock_file(home)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        return if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(format!(
                "lock {}: {err}",
                install_dir(home).join(".lock").display()
            ))
        };
    }
    Ok(Some(InstallLock(file)))
}

/// Downloads a manifest source straight into `path` (never a whole-body
/// buffer) and returns its sha256 hex; production uses
/// `http_fetch::get_to_file`, tests substitute a closure.
pub type Fetch<'a> = dyn Fn(&str, &Path, usize) -> Result<String, String> + 'a;

/// Extract exactly `member` from the gzip tarball at `archive` into
/// `dest_dir` with the system `tar`. The archive's hash was verified before
/// this runs; the member name is the manifest's, never the archive's own
/// listing, so a crafted archive cannot place anything else.
fn extract_member(archive: &Path, member: &str, dest_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("create {}: {e}", dest_dir.display()))?;
    let output = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest_dir)
        .arg("--")
        .arg(member)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("run tar: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "tar could not extract `{member}` from {} ({}): {}",
            archive.display(),
            output.status,
            stderr.trim()
        ));
    }
    let extracted = dest_dir.join(member);
    if !extracted.is_file() {
        return Err(format!(
            "archive {} does not contain `{member}`",
            archive.display()
        ));
    }
    Ok(extracted)
}

/// Install (or confirm) the pinned engine under `home` for `platform`, using
/// `fetch` for downloads. Returns the verified binary path. Holds the install
/// lock for the whole operation; on any failure nothing is left behind but
/// the previous managed copy (if any).
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
                "{}: cua-driver {} publishes no build for {}-{}",
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
                &format!("installing cua-driver {} ({reason})", manifest.version),
            );
        }
    }
    let source = manifest
        .source_for(platform)
        .ok_or_else(|| VerifyError::Unsupported.to_string())?;
    let dir = install_dir(home);
    let final_path = binary_path(home);
    let pid = std::process::id();
    // Stream the archive to a .part in the install dir (same filesystem as
    // the final path, so the last rename is atomic); the hash is computed
    // while streaming. The member is extracted into a private directory
    // beside it and verified on its own before it moves into place.
    let archive = dir.join(format!(".cua-driver.{pid}.part"));
    let extract_dir = dir.join(format!(".cua-driver.{pid}.extract"));
    let cleanup = || {
        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_dir_all(&extract_dir);
    };
    let actual = fetch(&source.url, &archive, MAX_ARCHIVE_BYTES).map_err(|e| {
        cleanup();
        format!("download {}: {e}", source.url)
    })?;
    if actual != source.sha256 {
        cleanup();
        let message = format!(
            "downloaded cua-driver {} for {platform} does not match the pinned sha256 \
(expected {}, got {actual}); nothing was installed",
            manifest.version, source.sha256
        );
        trace(home, &message);
        return Err(message);
    }
    let extracted = extract_member(&archive, &manifest.archive_member, &extract_dir)
        .inspect_err(|_| cleanup())?;
    let binary_actual = sha256_file(&extracted).map_err(|e| {
        cleanup();
        e.to_string()
    })?;
    if binary_actual != source.binary_sha256 {
        cleanup();
        let message = format!(
            "extracted cua-driver {} for {platform} does not match the pinned binarySha256 \
(expected {}, got {binary_actual}); nothing was installed",
            manifest.version, source.binary_sha256
        );
        trace(home, &message);
        return Err(message);
    }
    mark_executable(&extracted).inspect_err(|_| cleanup())?;
    std::fs::rename(&extracted, &final_path)
        .map_err(|e| format!("rename into {}: {e}", final_path.display()))
        .inspect_err(|_| cleanup())?;
    cleanup();
    if let Ok(mut guard) = VERIFIED.lock() {
        *guard = None;
    }
    let _ = std::fs::write(version_marker_path(home), format!("{}\n", manifest.version));
    ensure_notice(home, manifest, fetch);
    // The download went through a 64 KiB buffer, but the TLS session and
    // hasher still leave freed heap behind; hand it back so a worker that
    // installed on demand does not carry the pages in its footprint.
    crate::terminal_viewport::release_memory_to_os();
    trace(
        home,
        &format!(
            "installed cua-driver {} for {platform} at {} (archive sha256 {}, binary sha256 {})",
            manifest.version,
            final_path.display(),
            &actual[..12],
            &binary_actual[..12]
        ),
    );
    Ok(final_path)
}

/// `ensure_installed_with` against the embedded pin, this platform, and the
/// rustls HTTP helper.
pub fn ensure_installed(home: &Path) -> Result<PathBuf, String> {
    let platform = current_platform().ok_or_else(|| {
        format!(
            "no pinned computer-use engine for {}-{}",
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

/// The MIT notice next to the binary. Best effort: a missing notice never
/// fails an install (the trace line records it); a notice whose hash is
/// pinned is verified like the binary.
fn ensure_notice(home: &Path, manifest: &Manifest, fetch: &Fetch<'_>) {
    let path = notice_path(home);
    let Some(notice) = manifest.notice.as_ref() else {
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!(
                    "cua-driver {} is licensed under {}.\n",
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
        let bundled = dir.join("cua-driver");
        if bundled.is_file() {
            return Ok(bundled);
        }
    }
    for dir in path_dirs {
        let candidate = dir.join("cua-driver");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(missing_engine_message(home))
}

/// `resolve_with` from the process environment.
pub fn resolve(home: &Path) -> Result<PathBuf, String> {
    let env_override = std::env::var(ENV_OVERRIDE)
        .ok()
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

/// Debian/Ubuntu packages for the X11 client libraries the Linux engine
/// links dynamically (cua-driver is an X11 client even before a display
/// exists). A bare image lacks them, and the loader then refuses to start
/// the binary at all — `probe` turns that into a named reason.
pub const LINUX_RUNTIME_PACKAGES: &str =
    "libxi6 libxtst6 libx11-6 libxext6 libxrandr2 libxinerama1 libxcursor1 libxfixes3 libxkbcommon0 libxcb1";

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Why a resolved engine binary cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeFailure {
    pub binary: PathBuf,
    /// Shared libraries the dynamic loader (or `ldd`) reported missing, in
    /// first-seen order; empty when the failure was something else.
    pub missing_libs: Vec<String>,
    /// The loader/exec error text (bounded), for the trace and `--json`.
    pub detail: String,
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.missing_libs.is_empty() {
            write!(
                f,
                "cua-driver at {} cannot start: {}",
                self.binary.display(),
                self.detail
            )
        } else {
            write!(
                f,
                "cua-driver at {} cannot start: missing shared libraries {}. The Linux engine \
is an X11 client; on Debian/Ubuntu run `sudo apt-get install -y {LINUX_RUNTIME_PACKAGES}` and \
check again",
                self.binary.display(),
                self.missing_libs.join(", ")
            )
        }
    }
}

/// Run the resolved engine once (`--version`, bounded, telemetry off) and
/// return its version line. An exec failure or a non-zero exit is a
/// `ProbeFailure` naming the missing shared libraries when the loader (or
/// `ldd`, when present) says so — the case a bare container hits: the
/// install verified every hash, and the binary still cannot start.
pub fn probe(binary: &Path) -> Result<String, ProbeFailure> {
    let output = run_bounded(binary, &["--version"], PROBE_TIMEOUT);
    match output {
        Ok((true, stdout, _)) => {
            let line = stdout.lines().next().unwrap_or_default().trim().to_string();
            if line.is_empty() {
                return Err(ProbeFailure {
                    binary: binary.to_path_buf(),
                    missing_libs: Vec::new(),
                    detail: "`--version` printed nothing".into(),
                });
            }
            Ok(line)
        }
        Ok((false, stdout, stderr)) => {
            let text = format!("{stderr}\n{stdout}");
            let mut missing = missing_libs_from_loader_text(&text);
            if missing.is_empty() {
                missing = missing_libs_from_ldd(binary);
            }
            Err(ProbeFailure {
                binary: binary.to_path_buf(),
                missing_libs: missing,
                detail: bounded_detail(&text),
            })
        }
        Err(error) => Err(ProbeFailure {
            binary: binary.to_path_buf(),
            missing_libs: missing_libs_from_ldd(binary),
            detail: error,
        }),
    }
}

/// `(success, stdout, stderr)` of `binary args`, killed after `timeout`.
fn run_bounded(
    binary: &Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<(bool, String, String), String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary)
        .args(args)
        .env("CUA_DRIVER_EMBEDDED", "1")
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("exec failed: {e}"))?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{}` did not answer within {}s",
                    args.join(" "),
                    timeout.as_secs()
                ));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok((status.success(), stdout, stderr))
}

/// Shared-library names from a dynamic-loader error such as
/// `error while loading shared libraries: libXi.so.6: cannot open shared
/// object file` or an `ldd` line `libXtst.so.6 => not found`.
pub fn missing_libs_from_loader_text(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let candidate = if let Some(rest) = line.split("loading shared libraries:").nth(1) {
            rest.split(':').next().map(str::trim)
        } else if line.contains("not found") {
            line.split_whitespace().next()
        } else {
            None
        };
        if let Some(name) = candidate {
            let name = name.trim_matches(|c: char| c == '"' || c == '\'');
            if name.starts_with("lib") && name.contains(".so") && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Every library `ldd` reports as `not found` (Linux only; empty when `ldd`
/// is absent or the file is not a dynamic executable).
fn missing_libs_from_ldd(binary: &Path) -> Vec<String> {
    if std::env::consts::OS != "linux" {
        return Vec::new();
    }
    match run_bounded(
        Path::new("ldd"),
        &[&binary.display().to_string()],
        PROBE_TIMEOUT,
    ) {
        Ok((_, stdout, stderr)) => missing_libs_from_loader_text(&format!("{stdout}\n{stderr}")),
        Err(_) => Vec::new(),
    }
}

fn bounded_detail(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "exited with an error and no output".into();
    }
    let mut detail: String = trimmed.chars().take(300).collect();
    if detail.len() < trimmed.len() {
        detail.push('…');
    }
    detail
}

pub fn missing_engine_message(home: &Path) -> String {
    let managed = binary_path(home);
    let hint = match verify(home) {
        Err(VerifyError::Mismatch { installed_version }) => format!(
            "{} exists but does not match the pinned cua-driver {}{}",
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
        "The computer-use engine (cua-driver {}) is not available: {hint}. Run `unpeel computer \
install` on this Host (or set {ENV_OVERRIDE} to an engine binary).",
        pinned().version
    )
}

/// The desktop session visible to this process, as the daemon needs it:
/// a display and the session D-Bus the AT-SPI accessibility bus lives on.
/// `unpeel serve` and `unpeel computer install --check` share this check;
/// nothing here starts a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSession {
    /// `X11 :0` / `Wayland wayland-0` (`macOS (app-owned daemon)` on macOS).
    pub display: String,
    pub wayland: bool,
    /// The session bus address to hand the daemon (`unix:path=…`); `None`
    /// only on macOS, where the app owns the daemon and the question does
    /// not arise.
    pub session_bus: Option<String>,
}

impl DesktopSession {
    /// The environment the daemon child must see beyond what it inherits.
    pub fn daemon_env(&self) -> Vec<(&'static str, String)> {
        let mut env = Vec::new();
        if let Some(bus) = &self.session_bus {
            env.push(("DBUS_SESSION_BUS_ADDRESS", bus.clone()));
        }
        if self.wayland {
            // cua-driver's native Wayland backend is opt-in.
            env.push(("CUA_DRIVER_RS_ENABLE_WAYLAND", "1".to_string()));
        }
        env
    }
}

/// Resolve the desktop session from this process's environment. `Err`
/// names the missing piece (no display, or no session bus) so the Host's
/// unavailable reason and the CLI's exit-4 line say what to fix.
pub fn desktop_session() -> Result<DesktopSession, String> {
    let env = |name: &str| std::env::var(name).ok();
    desktop_session_from(
        env("WAYLAND_DISPLAY").as_deref(),
        env("XDG_RUNTIME_DIR").as_deref(),
        env("DISPLAY").as_deref(),
        env("DBUS_SESSION_BUS_ADDRESS").as_deref(),
        Some(unsafe { libc::getuid() }),
    )
}

/// Pure form of `desktop_session` for the Host, the CLI, and tests.
pub fn desktop_session_from(
    wayland_display: Option<&str>,
    runtime_dir: Option<&str>,
    display: Option<&str>,
    bus_env: Option<&str>,
    uid: Option<u32>,
) -> Result<DesktopSession, String> {
    if std::env::consts::OS == "macos" {
        return Ok(DesktopSession {
            display: "macOS (app-owned daemon)".into(),
            wayland: false,
            session_bus: None,
        });
    }
    let (display, wayland) = match graphical_session_from(wayland_display, runtime_dir, display) {
        Some(label) => {
            let wayland = label.starts_with("Wayland ");
            (label, wayland)
        }
        None => {
            return Err(
                "no graphical session is visible to this process: start it from the desktop \
session with DISPLAY (X11) or WAYLAND_DISPLAY and XDG_RUNTIME_DIR (Wayland) set"
                    .into(),
            )
        }
    };
    let session_bus = session_bus_address(bus_env, runtime_dir, uid).ok_or_else(|| {
        format!(
            "{display} is visible but no session D-Bus is: the accessibility (AT-SPI) bus the \
engine reads window trees from lives on it. Set DBUS_SESSION_BUS_ADDRESS, or start from a \
session whose user manager owns $XDG_RUNTIME_DIR/bus (`systemctl --user`)"
        )
    })?;
    Ok(DesktopSession {
        display,
        wayland,
        session_bus: Some(session_bus),
    })
}

/// Session-bus discovery in the order cua-driver itself uses:
/// `DBUS_SESSION_BUS_ADDRESS` → `$XDG_RUNTIME_DIR/bus` → `/run/user/<uid>/bus`
/// (the last two must exist as sockets).
pub fn session_bus_address(
    bus_env: Option<&str>,
    runtime_dir: Option<&str>,
    uid: Option<u32>,
) -> Option<String> {
    if let Some(value) = bus_env.map(str::trim).filter(|v| !v.is_empty()) {
        return Some(value.to_string());
    }
    let mut candidates = Vec::new();
    if let Some(dir) = runtime_dir.map(str::trim).filter(|d| !d.is_empty()) {
        candidates.push(PathBuf::from(dir).join("bus"));
    }
    if let Some(uid) = uid {
        candidates.push(PathBuf::from(format!("/run/user/{uid}/bus")));
    }
    candidates
        .into_iter()
        .find(|path| is_socket(path))
        .map(|path| format!("unix:path={}", path.display()))
}

fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false)
}

/// The desktop session's display label alone (`X11 :0`), or `None`; kept
/// for callers that only need to know whether a display exists.
pub fn graphical_session() -> Option<String> {
    graphical_session_from(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    )
}

pub fn graphical_session_from(
    wayland_display: Option<&str>,
    runtime_dir: Option<&str>,
    display: Option<&str>,
) -> Option<String> {
    if std::env::consts::OS == "macos" {
        return Some("macOS (app-owned daemon)".into());
    }
    if let Some(value) = wayland_display.map(str::trim).filter(|v| !v.is_empty()) {
        let has_runtime_dir = Path::new(value).is_absolute()
            || runtime_dir.map(str::trim).is_some_and(|d| !d.is_empty());
        if has_runtime_dir {
            return Some(format!("Wayland {value}"));
        }
    }
    display
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("X11 {v}"))
}

/// One sentence for the "no desktop session" case, with the doctor hint.
pub fn missing_session_message() -> String {
    match desktop_session() {
        Ok(_) => String::new(),
        Err(reason) => format!("{reason}; then check `cua-driver doctor --json` there."),
    }
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
            "{} computer-engine {message}",
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
            "unpeel-computer-engine-{tag}-{}-{}",
            std::process::id(),
            crate::state::current_timestamp_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real gzip tarball (made with the system `tar`, the same tool the
    /// installer extracts with) whose only member is `cua-driver` holding
    /// `engine`, plus an unrelated file so "exactly one member" is proven.
    fn archive_with(home: &Path, engine: &[u8]) -> (PathBuf, String, String) {
        let src = home.join("archive-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("cua-driver"), engine).unwrap();
        std::fs::write(src.join("cua-cursor-theme"), b"sidecar").unwrap();
        let archive = home.join("release.tar.gz");
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg("cua-driver")
            .arg("cua-cursor-theme")
            .status()
            .unwrap();
        assert!(status.success());
        let archive_sha = sha256_hex(&std::fs::read(&archive).unwrap());
        (archive, archive_sha, sha256_hex(engine))
    }

    fn manifest_for(archive_sha: &str, binary_sha: &str) -> Manifest {
        Manifest {
            version: "9.9.9".into(),
            license: "MIT".into(),
            notice: None,
            archive_member: "cua-driver".into(),
            sources: vec![Source {
                platform: "test-plat".into(),
                url: "https://example.invalid/cua-driver.tar.gz".into(),
                sha256: archive_sha.into(),
                binary_sha256: binary_sha.into(),
            }],
        }
    }

    /// A fetch that "downloads" by copying the prepared archive.
    fn fetch_copy(archive: PathBuf) -> impl Fn(&str, &Path, usize) -> Result<String, String> {
        move |_url, path, _max| {
            std::fs::copy(&archive, path).map_err(|e| e.to_string())?;
            Ok(sha256_hex(&std::fs::read(path).unwrap()))
        }
    }

    fn no_leftovers(home: &Path) -> bool {
        std::fs::read_dir(install_dir(home)).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.ends_with(".part") && !name.ends_with(".extract")
        })
    }

    #[test]
    fn embedded_manifest_is_valid_and_covers_every_platform() {
        let manifest = pinned();
        assert_eq!(manifest.archive_member, "cua-driver");
        assert!(manifest.notice.is_some());
        for platform in ["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"] {
            let source = manifest.source_for(platform).expect(platform);
            assert!(source.url.contains(&manifest.version), "{platform}");
            assert_ne!(source.sha256, source.binary_sha256, "{platform}");
        }
        // The two darwin entries deliberately share the universal archive;
        // the two linux entries must not.
        assert_eq!(
            manifest.source_for("darwin-arm64").unwrap().sha256,
            manifest.source_for("darwin-x64").unwrap().sha256
        );
        assert_ne!(
            manifest.source_for("linux-x64").unwrap().binary_sha256,
            manifest.source_for("linux-arm64").unwrap().binary_sha256
        );
    }

    #[test]
    fn manifest_rejects_malformed_entries() {
        let good = pinned();
        let mut json =
            serde_json::to_value(serde_json::from_str::<serde_json::Value>(MANIFEST_JSON).unwrap())
                .unwrap();
        json["sources"][0]["binarySha256"] = serde_json::Value::String("abc".into());
        assert!(Manifest::parse(&json.to_string())
            .unwrap_err()
            .contains("binarySha256"));
        let mut json = serde_json::from_str::<serde_json::Value>(MANIFEST_JSON).unwrap();
        json["sources"][0]["url"] = serde_json::Value::String("http://plain".into());
        assert!(Manifest::parse(&json.to_string())
            .unwrap_err()
            .contains("not https"));
        let mut json = serde_json::from_str::<serde_json::Value>(MANIFEST_JSON).unwrap();
        json["archiveMember"] = serde_json::Value::String("../cua-driver".into());
        assert!(Manifest::parse(&json.to_string())
            .unwrap_err()
            .contains("archiveMember"));
        let mut json = serde_json::from_str::<serde_json::Value>(MANIFEST_JSON).unwrap();
        json["notice"]["sha256"] = serde_json::Value::String("nope".into());
        assert!(Manifest::parse(&json.to_string())
            .unwrap_err()
            .contains("notice"));
        assert_eq!(good.version, pinned().version);
    }

    #[test]
    fn installs_exactly_the_member_from_a_verified_archive() {
        let home = temp_home("install");
        let engine = b"#!/bin/sh\necho cua-driver test\n";
        let (archive, archive_sha, binary_sha) = archive_with(&home, engine);
        let manifest = manifest_for(&archive_sha, &binary_sha);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        let copy = fetch_copy(archive);
        let fetch = move |url: &str, path: &Path, max: usize| {
            calls2.fetch_add(1, Ordering::SeqCst);
            copy(url, path, max)
        };
        let path = ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap();
        assert_eq!(path, binary_path(&home));
        assert_eq!(std::fs::read(&path).unwrap(), engine);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!install_dir(&home).join("cua-cursor-theme").exists());
        assert!(no_leftovers(&home));
        assert_eq!(
            std::fs::read_to_string(version_marker_path(&home)).unwrap(),
            "9.9.9\n"
        );
        assert!(std::fs::read_to_string(notice_path(&home))
            .unwrap()
            .contains("MIT"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Second call: verified, no download.
        ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(verify_with(&home, &manifest, "test-plat").unwrap(), path);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_archive_hash_mismatch_installs_nothing() {
        let home = temp_home("archive-mismatch");
        let (archive, _archive_sha, binary_sha) = archive_with(&home, b"engine");
        let manifest = manifest_for(&"0".repeat(64), &binary_sha);
        let err =
            ensure_installed_with(&home, &manifest, "test-plat", &fetch_copy(archive)).unwrap_err();
        assert!(err.contains("does not match the pinned sha256"), "{err}");
        assert!(!binary_path(&home).exists());
        assert!(no_leftovers(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_member_hash_mismatch_installs_nothing() {
        let home = temp_home("member-mismatch");
        let (archive, archive_sha, _binary_sha) = archive_with(&home, b"engine");
        let manifest = manifest_for(&archive_sha, &"1".repeat(64));
        let err =
            ensure_installed_with(&home, &manifest, "test-plat", &fetch_copy(archive)).unwrap_err();
        assert!(err.contains("binarySha256"), "{err}");
        assert!(!binary_path(&home).exists());
        assert!(no_leftovers(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_archive_without_the_member_installs_nothing() {
        let home = temp_home("no-member");
        let src = home.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("other"), b"x").unwrap();
        let archive = home.join("release.tar.gz");
        assert!(std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg("other")
            .status()
            .unwrap()
            .success());
        let archive_sha = sha256_hex(&std::fs::read(&archive).unwrap());
        let manifest = manifest_for(&archive_sha, &sha256_hex(b"x"));
        let err =
            ensure_installed_with(&home, &manifest, "test-plat", &fetch_copy(archive)).unwrap_err();
        assert!(err.contains("cua-driver"), "{err}");
        assert!(!binary_path(&home).exists());
        assert!(no_leftovers(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_failed_download_leaves_no_part_file() {
        let home = temp_home("failed-download");
        let manifest = manifest_for(&"2".repeat(64), &"3".repeat(64));
        let fetch = |_url: &str, path: &Path, _max: usize| {
            std::fs::write(path, b"half").unwrap();
            Err("connection reset".to_string())
        };
        let err = ensure_installed_with(&home, &manifest, "test-plat", &fetch).unwrap_err();
        assert!(err.contains("connection reset"), "{err}");
        assert!(!binary_path(&home).exists());
        assert!(no_leftovers(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_stale_managed_engine_is_skipped_then_replaced() {
        let home = temp_home("stale");
        std::fs::create_dir_all(install_dir(&home)).unwrap();
        std::fs::write(binary_path(&home), b"old engine").unwrap();
        std::fs::write(version_marker_path(&home), "0.12.2\n").unwrap();
        let (archive, archive_sha, binary_sha) = archive_with(&home, b"new engine");
        let manifest = manifest_for(&archive_sha, &binary_sha);
        assert_eq!(
            verify_with(&home, &manifest, "test-plat").unwrap_err(),
            VerifyError::Mismatch {
                installed_version: Some("0.12.2".into())
            }
        );
        // resolve_with skips the stale managed copy rather than using it
        // (verify() uses the real pin, which the fake bytes also fail).
        let err = resolve_with(None, &home, None, &[]).unwrap_err();
        assert!(err.contains("unpeel computer install"), "{err}");
        ensure_installed_with(&home, &manifest, "test-plat", &fetch_copy(archive)).unwrap();
        assert_eq!(std::fs::read(binary_path(&home)).unwrap(), b"new engine");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolution_order_is_override_managed_sibling_path() {
        let home = temp_home("resolve");
        let sibling_dir = home.join("bundle");
        let path_dir = home.join("path");
        std::fs::create_dir_all(&sibling_dir).unwrap();
        std::fs::create_dir_all(&path_dir).unwrap();
        let path_engine = path_dir.join("cua-driver");
        std::fs::write(&path_engine, b"path").unwrap();
        assert_eq!(
            resolve_with(
                None,
                &home,
                Some(&sibling_dir),
                std::slice::from_ref(&path_dir)
            )
            .unwrap(),
            path_engine
        );
        let sibling_engine = sibling_dir.join("cua-driver");
        std::fs::write(&sibling_engine, b"sibling").unwrap();
        assert_eq!(
            resolve_with(
                None,
                &home,
                Some(&sibling_dir),
                std::slice::from_ref(&path_dir)
            )
            .unwrap(),
            sibling_engine
        );
        let override_engine = home.join("override");
        std::fs::write(&override_engine, b"override").unwrap();
        assert_eq!(
            resolve_with(
                Some(override_engine.to_str().unwrap()),
                &home,
                Some(&sibling_dir),
                std::slice::from_ref(&path_dir)
            )
            .unwrap(),
            override_engine
        );
        // A dangling override falls through instead of failing.
        assert_eq!(
            resolve_with(
                Some("/nonexistent/cua-driver"),
                &home,
                Some(&sibling_dir),
                &[]
            )
            .unwrap(),
            sibling_engine
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unsupported_platform_is_a_clear_error() {
        let home = temp_home("unsupported");
        let manifest = manifest_for(&"4".repeat(64), &"5".repeat(64));
        let fetch = |_url: &str, _path: &Path, _max: usize| Ok(String::new());
        let err = ensure_installed_with(&home, &manifest, "nope-plat", &fetch).unwrap_err();
        assert!(err.contains("publishes no build"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_opt_out_parses_like_the_browser_engine() {
        assert!(install_enabled_from(None));
        assert!(install_enabled_from(Some("")));
        assert!(install_enabled_from(Some("1")));
        for off in ["0", "false", "OFF", " no "] {
            assert!(!install_enabled_from(Some(off)), "{off}");
        }
    }

    #[test]
    fn graphical_session_needs_a_display_or_a_resolvable_wayland_socket() {
        if std::env::consts::OS == "macos" {
            assert!(graphical_session_from(None, None, None).is_some());
            return;
        }
        assert_eq!(graphical_session_from(None, None, None), None);
        assert_eq!(graphical_session_from(None, None, Some(" ")), None);
        assert_eq!(
            graphical_session_from(None, None, Some(":0")).as_deref(),
            Some("X11 :0")
        );
        assert_eq!(
            graphical_session_from(Some("wayland-0"), None, Some(":0")).as_deref(),
            Some("X11 :0"),
            "a relative Wayland socket without a runtime dir is unusable"
        );
        assert_eq!(
            graphical_session_from(Some("wayland-0"), Some("/run/user/1000"), None).as_deref(),
            Some("Wayland wayland-0")
        );
        assert_eq!(
            graphical_session_from(Some("/tmp/wl"), None, None).as_deref(),
            Some("Wayland /tmp/wl")
        );
    }

    #[test]
    fn session_bus_discovery_prefers_env_then_runtime_dir_then_run_user() {
        let home = temp_home("bus");
        assert_eq!(
            session_bus_address(Some(" unix:path=/x "), None, None).as_deref(),
            Some("unix:path=/x")
        );
        // A runtime dir whose bus is a plain file (not a socket) is skipped.
        let runtime = home.join("rt");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("bus"), b"not a socket").unwrap();
        assert_eq!(
            session_bus_address(Some(""), runtime.to_str(), Some(4_000_000_000)),
            None
        );
        let _ = std::fs::remove_file(runtime.join("bus"));
        let listener = std::os::unix::net::UnixListener::bind(runtime.join("bus")).unwrap();
        assert_eq!(
            session_bus_address(None, runtime.to_str(), None).as_deref(),
            Some(format!("unix:path={}", runtime.join("bus").display()).as_str())
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn desktop_session_names_the_missing_piece() {
        if std::env::consts::OS == "macos" {
            let session = desktop_session_from(None, None, None, None, None).unwrap();
            assert!(session.session_bus.is_none() && !session.wayland);
            assert!(session.daemon_env().is_empty());
            return;
        }
        let err = desktop_session_from(None, None, None, None, None).unwrap_err();
        assert!(err.contains("no graphical session"), "{err}");
        let err =
            desktop_session_from(None, None, Some(":0"), None, Some(4_000_000_000)).unwrap_err();
        assert!(err.contains("no session D-Bus"), "{err}");
        let session =
            desktop_session_from(None, None, Some(":0"), Some("unix:path=/b"), None).unwrap();
        assert_eq!(session.display, "X11 :0");
        assert!(!session.wayland);
        assert_eq!(
            session.daemon_env(),
            vec![("DBUS_SESSION_BUS_ADDRESS", "unix:path=/b".to_string())]
        );
        let session = desktop_session_from(
            Some("wayland-0"),
            Some("/run/user/1"),
            None,
            Some("unix:path=/b"),
            None,
        )
        .unwrap();
        assert!(session.wayland);
        assert!(session
            .daemon_env()
            .contains(&("CUA_DRIVER_RS_ENABLE_WAYLAND", "1".to_string())));
    }

    fn fake_engine(home: &Path, name: &str, script: &str) -> PathBuf {
        let path = home.join(name);
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn probe_names_missing_shared_libraries_from_the_loader_error() {
        let home = temp_home("probe-libs");
        // What a bare Bullseye image printed for the real linux-x86_64 engine.
        let engine = fake_engine(
            &home,
            "cua-driver",
            "#!/bin/sh\necho \"$0: error while loading shared libraries: libXi.so.6: cannot open shared object file: No such file or directory\" >&2\nexit 127\n",
        );
        let err = probe(&engine).unwrap_err();
        assert_eq!(err.missing_libs, vec!["libXi.so.6".to_string()]);
        assert!(err.detail.contains("libXi.so.6"), "{}", err.detail);
        let text = err.to_string();
        assert!(
            text.contains("missing shared libraries libXi.so.6"),
            "{text}"
        );
        assert!(text.contains(LINUX_RUNTIME_PACKAGES), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn probe_accepts_an_engine_that_answers_version() {
        let home = temp_home("probe-ok");
        let engine = fake_engine(&home, "cua-driver", "#!/bin/sh\necho 'cua-driver 9.9.9'\n");
        assert_eq!(probe(&engine).unwrap(), "cua-driver 9.9.9");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn probe_reports_other_exec_failures_without_inventing_libraries() {
        let home = temp_home("probe-exec");
        let err = probe(&home.join("does-not-exist")).unwrap_err();
        assert!(err.missing_libs.is_empty());
        assert!(err.detail.contains("exec failed"), "{}", err.detail);
        assert!(err.to_string().contains("cannot start"));
        let silent = fake_engine(&home, "silent", "#!/bin/sh\nexit 3\n");
        let err = probe(&silent).unwrap_err();
        assert!(err.missing_libs.is_empty());
        assert!(err.detail.contains("no output"), "{}", err.detail);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn loader_and_ldd_text_yield_unique_library_names() {
        let text = "\tlibXtst.so.6 => not found\n\tlibc.so.6 => /lib/libc.so.6 (0x1)\n\tlibXi.so.6 => not found\n./cua-driver: error while loading shared libraries: libXi.so.6: cannot open shared object file: No such file or directory\n";
        assert_eq!(
            missing_libs_from_loader_text(text),
            vec!["libXtst.so.6".to_string(), "libXi.so.6".to_string()]
        );
        assert!(missing_libs_from_loader_text("plain failure").is_empty());
    }

    #[test]
    fn a_second_installer_waits_for_the_lock() {
        let home = temp_home("lock");
        let held = lock(&home).unwrap();
        assert!(try_lock(&home).unwrap().is_none(), "lock must be exclusive");
        drop(held);
        // A blocking `lock` (what every real installer uses), not `try_lock`:
        // a sibling test's `tar` child forked while `held` was open inherits
        // that descriptor until its exec closes it, so the flock can outlive
        // `drop` by a millisecond. The waiter must simply get it next.
        let start = std::time::Instant::now();
        let reacquired = lock(&home).unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        drop(reacquired);
        let _ = std::fs::remove_dir_all(&home);
    }
}
