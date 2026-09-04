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

pub fn binary_path(home: &Path, app: &CatalogApp) -> PathBuf {
    install_dir(home).join(&app.binary)
}

fn release_target() -> Option<&'static str> {
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

fn release_url(app: &CatalogApp, target: &str) -> String {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub id: String,
    pub name: String,
    pub state: String,
    pub command: String,
    pub media_types: Vec<String>,
    pub file_extensions: std::collections::BTreeMap<String, String>,
    pub resource_kinds: Vec<String>,
    pub default_for: Vec<String>,
    pub path: Option<PathBuf>,
}

pub fn status(home: &Path, app: &CatalogApp) -> AppStatus {
    let managed = binary_path(home, app);
    let path = managed.is_file().then_some(managed).or_else(|| {
        crate::setup::find_command_path(&app.binary, &crate::setup::search_dirs())
            .map(PathBuf::from)
    });
    AppStatus {
        id: app.id.clone(),
        name: app.name.clone(),
        state: if path.is_some() { "ready" } else { "missing" }.into(),
        command: app.binary.clone(),
        media_types: app.media_types.clone(),
        file_extensions: app.file_extensions.clone(),
        resource_kinds: app.resource_kinds.clone(),
        default_for: app.default_for.clone(),
        path,
    }
}

pub fn catalog_wire() -> Value {
    let installed = apps_mcp::installed_apps()
        .into_iter()
        .map(|app| app.id)
        .collect::<std::collections::HashSet<_>>();
    Value::Array(
        apps_mcp::catalog_apps()
            .into_iter()
            .map(|app| {
                json!({
                    "id": app.id,
                    "name": app.name,
                    "description": app.description,
                    "tint": app.tint,
                    "command": app.binary,
                    "mediaTypes": app.media_types,
                    "fileExtensions": app.file_extensions,
                    "resourceKinds": app.resource_kinds,
                    "defaultFor": app.default_for,
                    "installed": installed.contains(&app.id),
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

    fn app() -> CatalogApp {
        CatalogApp {
            slug: "markdown".into(),
            id: "unpeel.app.markdown".into(),
            binary: "unpeel-markdown".into(),
            name: "Markdown".into(),
            channel: "stable".into(),
            description: String::new(),
            tint: None,
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
