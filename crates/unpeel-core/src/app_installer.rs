//! Host-owned installation and bootstrap projection for official Unpeel Apps.
//!
//! Apps execute beside the file and Session they operate on, so installation
//! belongs to the Host (including Linux and SSH Hosts), never the Controller.
//! The embedded registry is the allowlist; callers may select an id but cannot
//! supply a URL, binary name, archive member, or destination.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{io::Read, process::Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::apps_mcp::{self, CatalogApp};

const DEFAULT_BASE_URL: &str = "https://unpeel.com";
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SIDECAR_BYTES: usize = 4 * 1024;

pub fn install_dir(home: &Path) -> PathBuf {
    home.join("apps").join("bin")
}

/// Where this home resolves installed Apps from, in order: its own slot,
/// then the machine's default home (`~/.unpeel/apps/bin`) when this is an
/// isolated workspace home. Apps are per-Mac installs: a sibling workspace
/// on the same machine sees — and runs — what the default workspace
/// installed or dev-linked, instead of reporting it missing.
pub fn install_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![install_dir(home)];
    let machine = install_dir(&crate::app_paths::real_unpeel_home());
    if machine != dirs[0] {
        dirs.push(machine);
    }
    dirs
}

/// The installed binary this home would run for `app`: its own slot first,
/// else the machine's.
pub fn resolved_binary_path(home: &Path, app: &CatalogApp) -> Option<PathBuf> {
    install_dirs(home)
        .into_iter()
        .map(|dir| dir.join(&app.binary))
        .find(|path| path.is_file())
}

pub fn binary_path(home: &Path, app: &CatalogApp) -> PathBuf {
    install_dir(home).join(&app.binary)
}

/// What the Host knows about each installed App (`~/.unpeel/apps/installed.json`,
/// keyed by App id). Written by the installer and by `link`; a copy that
/// predates this record reads as version-unknown and is offered an update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InstalledRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub installed_at_unix_ms: u64,
    /// `release` (a verified download) or `link` (a developer's local build).
    #[serde(default)]
    pub source: String,
}

fn installed_records_path(home: &Path) -> PathBuf {
    home.join("apps").join("installed.json")
}

pub fn installed_records(home: &Path) -> std::collections::BTreeMap<String, InstalledRecord> {
    std::fs::read(installed_records_path(home))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

/// Replace (or with `None`, drop) one App's record; atomic rename so a reader
/// never sees a torn file. Callers hold the install lock.
fn write_installed_record(home: &Path, app_id: &str, record: Option<InstalledRecord>) {
    let mut records = installed_records(home);
    match record {
        Some(record) => {
            records.insert(app_id.to_string(), record);
        }
        None => {
            records.remove(app_id);
        }
    }
    let path = installed_records_path(home);
    let staged = path.with_extension(format!("json.{}.part", std::process::id()));
    if let Ok(raw) = serde_json::to_vec_pretty(&records) {
        if std::fs::write(&staged, raw).is_ok() {
            let _ = std::fs::rename(&staged, &path);
        }
        let _ = std::fs::remove_file(&staged);
    }
}

pub(crate) fn release_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => Some("macos-universal"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        _ => None,
    }
}

fn base_url() -> String {
    std::env::var("UNPEEL_INSTALL_BASE")
        .ok()
        .map(|value| value.trim_end_matches('/').to_owned())
        .filter(|value| value.starts_with("https://") || value.starts_with("http://localhost"))
        .unwrap_or_else(|| DEFAULT_BASE_URL.into())
}

pub(crate) fn release_url(app: &CatalogApp, target: &str) -> String {
    let channel = std::env::var("UNPEEL_CHANNEL")
        .ok()
        .filter(|value| matches!(value.as_str(), "alpha" | "beta" | "stable"))
        .unwrap_or_else(|| app.channel.clone());
    format!(
        "{}/releases/{}/{}/{}-latest-{target}.tar.gz",
        base_url(),
        channel,
        app.slug,
        app.binary
    )
}

struct InstallLock(#[allow(dead_code)] std::fs::File);

fn lock(home: &Path) -> Result<InstallLock, String> {
    use std::os::fd::AsRawFd;

    let dir = install_dir(home);
    std::fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let path = dir.join(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "lock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(InstallLock(file))
}

fn expected_digest(path: &Path) -> Result<String, String> {
    let sidecar = std::fs::read_to_string(path)
        .map_err(|error| format!("read checksum sidecar {}: {error}", path.display()))?;
    let digest = sidecar.split_whitespace().next().unwrap_or_default();
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("checksum sidecar does not begin with a SHA-256 digest".into());
    }
    Ok(digest.to_ascii_lowercase())
}

fn mark_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

/// Downloads an object to `path` and returns its streamed SHA-256.
pub type Fetch<'a> = dyn Fn(&str, &Path, usize) -> Result<String, String> + 'a;

/// Install one official App using an injectable fetcher. The archive may
/// contain other files, but only the allowlisted root member is extracted.
/// The previous working binary remains in place on every failure path.
pub fn install_with(
    home: &Path,
    app: &CatalogApp,
    target: &str,
    fetch: &Fetch<'_>,
) -> Result<PathBuf, String> {
    let _lock = lock(home)?;
    let dir = install_dir(home);
    let nonce = std::process::id();
    let archive = dir.join(format!(".{}.{}.tar.gz.part", app.binary, nonce));
    let sidecar = dir.join(format!(".{}.{}.sha256.part", app.binary, nonce));
    let extracted = dir.join(format!(".{}.{}.binary.part", app.binary, nonce));
    let cleanup = || {
        let _ = std::fs::remove_file(&archive);
        let _ = std::fs::remove_file(&sidecar);
        let _ = std::fs::remove_file(&extracted);
    };

    let url = release_url(app, target);
    if let Err(error) = fetch(&format!("{url}.sha256"), &sidecar, MAX_SIDECAR_BYTES) {
        cleanup();
        return Err(format!("download checksum sidecar {url}.sha256: {error}"));
    }
    let expected = expected_digest(&sidecar).inspect_err(|_| cleanup())?;
    let actual = fetch(&url, &archive, MAX_ARCHIVE_BYTES).map_err(|error| {
        cleanup();
        format!("download {url}: {error}")
    })?;
    if actual.to_ascii_lowercase() != expected {
        cleanup();
        return Err(format!(
            "downloaded {} does not match its checksum; nothing was installed",
            app.name
        ));
    }

    let mut child = Command::new("tar")
        .args(["-xzOf"])
        .arg(&archive)
        .arg(&app.binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            cleanup();
            format!("run tar: {error}")
        })?;
    let mut binary = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "capture tar output".to_string())?
        .take(MAX_ARCHIVE_BYTES as u64 + 1)
        .read_to_end(&mut binary)
        .map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            cleanup();
            format!("extract {}: {error}", app.binary)
        })?;
    if binary.len() > MAX_ARCHIVE_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        cleanup();
        return Err(format!("{} is unreasonably large", app.binary));
    }
    let status = child.wait().map_err(|error| {
        cleanup();
        format!("wait for tar: {error}")
    })?;
    if !status.success() || binary.is_empty() {
        cleanup();
        return Err(format!(
            "{} is missing from the downloaded archive",
            app.binary
        ));
    }
    std::fs::write(&extracted, binary).map_err(|error| {
        cleanup();
        format!("write {}: {error}", extracted.display())
    })?;
    mark_executable(&extracted).inspect_err(|_| cleanup())?;
    let destination = binary_path(home, app);
    std::fs::rename(&extracted, &destination).map_err(|error| {
        cleanup();
        format!("install {}: {error}", destination.display())
    })?;
    cleanup();
    write_installed_record(
        home,
        &app.id,
        Some(InstalledRecord {
            version: app.version.clone(),
            sha256: Some(expected),
            installed_at_unix_ms: crate::state::current_timestamp_ms(),
            source: "release".into(),
        }),
    );
    Ok(destination)
}

pub fn install(home: &Path, app_id: &str) -> Result<PathBuf, String> {
    let app = apps_mcp::catalog_app(app_id)
        .ok_or_else(|| format!("unknown or unsupported App id {app_id:?}"))?;
    let target = release_target().ok_or_else(|| {
        format!(
            "Unpeel Apps publish no build for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let installed = install_with(home, &app, target, &|url, path, max| {
        crate::http_fetch::get_to_file(url, path, max).map(|(_, digest)| digest)
    })?;
    crate::state_bus::announce(crate::state_bus::Change::AppState, None);
    Ok(installed)
}

/// Development mode: point the Host's managed slot for an official App at a
/// local build instead of a downloaded release. The Host resolves
/// `~/.unpeel/apps/bin/<binary>` first, so a symlink there wins over PATH
/// and over any installed copy — and follows every `cargo build`, since
/// the toolchain replaces the target as a new inode. Nothing is verified:
/// this is the developer's own binary on the developer's own machine.
pub fn link(home: &Path, app_id: &str, executable: &Path) -> Result<PathBuf, String> {
    let app = apps_mcp::catalog_app(app_id)
        .ok_or_else(|| format!("unknown or unsupported App id {app_id:?}"))?;
    if !executable.is_absolute() {
        return Err("the executable must be an absolute path".into());
    }
    let metadata = std::fs::metadata(executable)
        .map_err(|error| format!("cannot read {}: {error}", executable.display()))?;
    #[cfg(unix)]
    let runnable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let runnable = metadata.is_file();
    if !runnable {
        return Err(format!(
            "{} is not an executable file",
            executable.display()
        ));
    }
    let dir = install_dir(home);
    std::fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let target = binary_path(home, &app);
    // Replace whatever is there (a downloaded copy or an older link) through
    // a fresh symlink renamed into place, so a running instance keeps its
    // old inode and a new launch sees the new one.
    let staged = dir.join(format!(".{}.link.part", app.binary));
    let _ = std::fs::remove_file(&staged);
    #[cfg(unix)]
    std::os::unix::fs::symlink(executable, &staged)
        .map_err(|error| format!("link {}: {error}", staged.display()))?;
    #[cfg(not(unix))]
    return Err("linking Apps is only supported on Unix".into());
    std::fs::rename(&staged, &target)
        .map_err(|error| format!("replace {}: {error}", target.display()))?;
    write_installed_record(
        home,
        &app.id,
        Some(InstalledRecord {
            version: None,
            sha256: None,
            installed_at_unix_ms: crate::state::current_timestamp_ms(),
            source: "link".into(),
        }),
    );
    crate::state_bus::announce(crate::state_bus::Change::AppState, None);
    Ok(target)
}

/// Undo `link`: remove the managed slot only when it is a symlink, so a
/// downloaded release is never deleted by the dev-mode verb.
pub fn unlink(home: &Path, app_id: &str) -> Result<bool, String> {
    let app = apps_mcp::catalog_app(app_id)
        .ok_or_else(|| format!("unknown or unsupported App id {app_id:?}"))?;
    let target = binary_path(home, &app);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            std::fs::remove_file(&target)
                .map_err(|error| format!("remove {}: {error}", target.display()))?;
            if let Ok(_lock) = lock(home) {
                write_installed_record(home, &app.id, None);
            }
            crate::state_bus::announce(crate::state_bus::Change::AppState, None);
            Ok(true)
        }
        Ok(_) => Err(format!(
            "{} is an installed release, not a link; leave it or reinstall over it",
            target.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect {}: {error}", target.display())),
    }
}

/// True when the resolved managed binary is a dev-mode link.
pub fn is_linked(home: &Path, app: &CatalogApp) -> bool {
    resolved_binary_path(home, app)
        .and_then(|path| std::fs::symlink_metadata(path).ok())
        .is_some_and(|metadata| metadata.file_type().is_symlink())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub id: String,
    pub name: String,
    pub state: String,
    /// The registry's version for this App.
    pub version: Option<String>,
    /// What the Host recorded when it installed the copy in its slot; `None`
    /// for a copy that predates the record, a linked build, or PATH-found one.
    pub installed_version: Option<String>,
    /// A release copy sits in the slot and the registry publishes a version
    /// it does not match (or the copy is too old to have recorded one).
    pub update_available: bool,
    pub command: String,
    pub media_types: Vec<String>,
    pub file_extensions: std::collections::BTreeMap<String, String>,
    pub resource_kinds: Vec<String>,
    pub default_for: Vec<String>,
    pub path: Option<PathBuf>,
}

pub fn status(home: &Path, app: &CatalogApp) -> AppStatus {
    let managed = resolved_binary_path(home, app);
    let managed_present = managed.is_some();
    let path = managed.or_else(|| {
        crate::setup::find_command_path(&app.binary, &crate::setup::search_dirs())
            .map(PathBuf::from)
    });
    let linked = is_linked(home, app);
    let record = install_dirs(home).into_iter().find_map(|dir| {
        let home = dir.parent()?.parent()?;
        installed_records(home).remove(&app.id)
    });
    let installed_version = record
        .as_ref()
        .filter(|record| record.source == "release")
        .and_then(|record| record.version.clone());
    let update_available =
        managed_present && !linked && app.version.is_some() && installed_version != app.version;
    AppStatus {
        id: app.id.clone(),
        name: app.name.clone(),
        state: if path.is_none() {
            "missing"
        } else if linked {
            "linked"
        } else {
            "ready"
        }
        .into(),
        version: app.version.clone(),
        installed_version,
        update_available,
        command: app.binary.clone(),
        media_types: app.media_types.clone(),
        file_extensions: app.file_extensions.clone(),
        resource_kinds: app.resource_kinds.clone(),
        default_for: app.default_for.clone(),
        path,
    }
}

pub fn catalog_wire() -> Value {
    let installed = apps_mcp::discovered_apps()
        .into_iter()
        .map(|app| app.id)
        .collect::<std::collections::HashSet<_>>();
    let home = crate::app_paths::unpeel_home();
    // Shell startup files may put an older CLI first in PATH. Use the CLI
    // shipped beside this Host, including a remote Host's own absolute path.
    let cli = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("unpeel")))
        .filter(|path| path.is_file());
    Value::Array(
        apps_mcp::catalog_apps()
            .into_iter()
            .map(|app| {
                let status = status(&home, &app);
                json!({
                    "id": app.id,
                    "name": app.name,
                    "description": app.description,
                    "tint": app.tint,
                    "iconSvg": app.icon_svg,
                    "version": app.version,
                    "installedVersion": status.installed_version,
                    "updateAvailable": status.update_available,
                    "command": app.binary,
                    "mediaTypes": app.media_types,
                    "fileExtensions": app.file_extensions,
                    "resourceKinds": app.resource_kinds,
                    "defaultFor": app.default_for,
                    "installed": installed.contains(&app.id),
                    "installCommand": cli.as_ref().map(|cli| format!("{} apps {} {} --yes",
                        crate::integrations::shared::shell_quote(&cli.to_string_lossy()),
                        if installed.contains(&app.id) { "update" } else { "install" },
                        crate::integrations::shared::shell_quote(&app.id))),
                })
            })
            .collect(),
    )
}

pub fn installed_wire() -> Value {
    Value::Array(
        apps_mcp::installed_apps()
            .into_iter()
            .filter_map(|app| {
                Some(json!({
                    "id": app.id,
                    "name": app.name,
                    "description": app.description,
                    "tint": app.tint,
                    "iconSvg": app.icon_svg,
                    "command": app.command?,
                    "mediaTypes": app.media_types,
                    "fileExtensions": app.file_extensions,
                    "resourceKinds": app.resource_kinds,
                    "defaultFor": app.default_for,
                    "installed": true,
                }))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_points_the_managed_slot_at_a_local_build_and_unlink_only_removes_links() {
        let home = tempfile::tempdir().unwrap();
        let build = tempfile::tempdir().unwrap();
        let exe = build.path().join("unpeel-filetree");
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let linked = link(home.path(), "unpeel.app.filetree", &exe).unwrap();
        assert_eq!(linked, home.path().join("apps/bin/unpeel-filetree"));
        assert_eq!(std::fs::read_link(&linked).unwrap(), exe);
        let app = apps_mcp::catalog_app("unpeel.app.filetree").unwrap();
        assert!(is_linked(home.path(), &app));
        assert_eq!(status(home.path(), &app).state, "linked");

        // Relinking replaces in place; a relative or missing target is refused.
        link(home.path(), "unpeel.app.filetree", &exe).unwrap();
        assert!(link(
            home.path(),
            "unpeel.app.filetree",
            Path::new("target/release/x")
        )
        .is_err());
        assert!(link(
            home.path(),
            "unpeel.app.filetree",
            &build.path().join("nope")
        )
        .is_err());
        assert!(link(home.path(), "unpeel.app.nope", &exe).is_err());

        assert!(unlink(home.path(), "unpeel.app.filetree").unwrap());
        assert!(!linked.exists());
        assert!(!unlink(home.path(), "unpeel.app.filetree").unwrap());

        // A real (non-link) file in the slot is never removed by unlink.
        std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
        std::fs::write(&linked, b"release").unwrap();
        assert!(unlink(home.path(), "unpeel.app.filetree").is_err());
        assert!(linked.exists());
    }

    fn app() -> CatalogApp {
        CatalogApp {
            slug: "markdown".into(),
            id: "unpeel.app.markdown".into(),
            binary: "unpeel-markdown".into(),
            name: "Markdown".into(),
            version: Some("0.1.0".into()),
            channel: "stable".into(),
            description: String::new(),
            tint: None,
            icon_svg: None,
            media_types: vec!["text/markdown".into()],
            file_extensions: [("md".into(), "text/markdown".into())]
                .into_iter()
                .collect(),
            resource_kinds: vec!["folder".into()],
            default_for: vec!["file:text/markdown".into()],
        }
    }

    #[test]
    fn malformed_sidecar_never_replaces_an_existing_binary() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(install_dir(root.path())).unwrap();
        std::fs::write(binary_path(root.path(), &app()), b"old").unwrap();
        let error = install_with(root.path(), &app(), "test", &|url, path, _| {
            if url.ends_with(".sha256") {
                std::fs::write(path, b"not-a-digest\n").unwrap();
            }
            Ok(crate::browser_engine::sha256_hex(b"unused"))
        })
        .unwrap_err();
        assert!(error.contains("checksum sidecar"));
        assert_eq!(
            std::fs::read(binary_path(root.path(), &app())).unwrap(),
            b"old"
        );
    }

    #[test]
    fn verified_archive_installs_only_the_allowlisted_binary() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("unpeel-markdown"), b"new binary").unwrap();
        std::fs::write(source.path().join("ignored"), b"not installed").unwrap();
        let archive = source.path().join("release.tar.gz");
        assert!(Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .args(["-C"])
            .arg(source.path())
            .args(["unpeel-markdown", "ignored"])
            .status()
            .unwrap()
            .success());
        let digest = crate::browser_engine::sha256_hex(&std::fs::read(&archive).unwrap());
        let sidecar = source.path().join("release.sha256");
        std::fs::write(&sidecar, format!("{digest}  release.tar.gz\n")).unwrap();

        let installed = install_with(root.path(), &app(), "test", &|url, path, _| {
            let from = if url.ends_with(".sha256") {
                &sidecar
            } else {
                &archive
            };
            std::fs::copy(from, path).unwrap();
            Ok(crate::browser_engine::sha256_hex(
                &std::fs::read(path).unwrap(),
            ))
        })
        .unwrap();
        assert_eq!(std::fs::read(&installed).unwrap(), b"new binary");
        assert!(!install_dir(root.path()).join("ignored").exists());
    }

    #[test]
    fn catalog_wire_includes_missing_apps() {
        let wire = catalog_wire();
        let markdown = wire
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "unpeel.app.markdown")
            .unwrap();
        assert_eq!(markdown["command"], "unpeel-markdown");
        assert_eq!(markdown["mediaTypes"][0], "text/markdown");
        assert_eq!(markdown["fileExtensions"]["md"], "text/markdown");
    }
}
