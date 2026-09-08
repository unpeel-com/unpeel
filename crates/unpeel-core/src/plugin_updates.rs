//! Lazy, read-only update checks on the selected Host. Bootstrap never starts
//! network requests or version processes. Controllers poll this small endpoint
//! while Settings is visible; completed checks are cached per installation.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::runtime_catalog::RuntimeUpdates;

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const RETRY_TTL: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_VERSION_OUTPUT: usize = 8192;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Update {
    id: String,
    state: &'static str,
    installed_version: Option<String>,
    latest_version: Option<String>,
    update_available: bool,
}

impl Update {
    fn new(id: &str, state: &'static str, installed_version: Option<String>) -> Self {
        Self {
            id: id.into(),
            state,
            installed_version,
            latest_version: None,
            update_available: false,
        }
    }
}

#[derive(Clone, Debug)]
enum Source {
    App {
        checksum_url: String,
        digest: String,
    },
    Agent(RuntimeUpdates),
    Unknown,
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    binary: PathBuf,
    installed_version: Option<String>,
    source: Source,
    fingerprint: String,
}

impl Candidate {
    fn new(id: String, binary: PathBuf, installed_version: Option<String>, source: Source) -> Self {
        use std::os::unix::fs::MetadataExt;
        // Follow symlinks: a native agent updater replaces the version target.
        // A changed install record also invalidates cached App checks.
        let stamp = std::fs::metadata(&binary)
            .ok()
            .map(|meta| (meta.dev(), meta.ino(), meta.len(), meta.modified().ok()));
        let fingerprint = format!("{binary:?}:{stamp:?}:{source:?}:{installed_version:?}");
        Self {
            id,
            binary,
            installed_version,
            source,
            fingerprint,
        }
    }
}

struct Entry {
    fingerprint: String,
    checked_at: Instant,
    update: Update,
}

type Key = (PathBuf, String);
static CACHE: LazyLock<Mutex<HashMap<Key, Entry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn candidates(home: &Path) -> Vec<Candidate> {
    let dirs = crate::setup::search_dirs();
    let mut candidates = Vec::new();
    for runtime in crate::runtime_catalog::builtin_runtime_catalog().current_platform_descriptors()
    {
        if runtime.display.kind != crate::runtime_catalog::RuntimeKind::Agent {
            continue;
        }
        let Some(binary) = runtime
            .detection
            .command_aliases
            .iter()
            .find_map(|alias| crate::setup::find_command_path(alias, &dirs))
        else {
            continue;
        };
        candidates.push(Candidate::new(
            runtime.id.clone(),
            binary.into(),
            None,
            runtime
                .updates
                .clone()
                .map(resolve_channel)
                .map(Source::Agent)
                .unwrap_or(Source::Unknown),
        ));
    }
    for app in crate::apps_mcp::catalog_apps() {
        let Some(binary) = crate::app_installer::resolved_binary_path(home, &app)
            .or_else(|| crate::setup::find_command_path(&app.binary, &dirs).map(PathBuf::from))
        else {
            continue;
        };
        // The record must belong to the actual selected slot, never a sibling
        // workspace's copy with the same App id.
        let record = binary
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(|root| crate::app_installer::installed_records(root).remove(&app.id));
        let source = record
            .as_ref()
            .filter(|record| record.source == "release")
            .and_then(|record| record.sha256.as_ref())
            .filter(|digest| valid_digest(digest))
            .zip(crate::app_installer::release_target())
            .filter(|_| !crate::app_installer::is_linked(home, &app))
            .map(|(digest, target)| Source::App {
                checksum_url: format!("{}.sha256", crate::app_installer::release_url(&app, target)),
                digest: digest.to_ascii_lowercase(),
            })
            .unwrap_or(Source::Unknown);
        candidates.push(Candidate::new(
            app.id,
            binary,
            record.and_then(|record| record.version),
            source,
        ));
    }
    candidates
}

fn resolve_channel(mut recipe: RuntimeUpdates) -> RuntimeUpdates {
    let configured = recipe
        .channel_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok())
        .or_else(|| {
            let path = dirs::home_dir()?.join(recipe.channel_settings_path.as_ref()?);
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(64 * 1024)
                .read_to_end(&mut bytes)
                .ok()?;
            serde_json::from_slice::<Value>(&bytes)
                .ok()?
                .pointer(recipe.channel_settings_pointer.as_ref()?)?
                .as_str()
                .map(str::to_owned)
        });
    if let Some(url) = configured
        .as_ref()
        .and_then(|channel| recipe.channel_urls.get(channel))
    {
        recipe.latest_url = url.clone();
    }
    recipe
}

/// Called only by the capability-gated settings read, never by bootstrap.
pub fn request() -> Value {
    let home = crate::app_paths::unpeel_home();
    request_with(&home, candidates(&home))
}

fn request_with(home: &Path, candidates: Vec<Candidate>) -> Value {
    let mut jobs = Vec::new();
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    let mut items = Vec::new();
    for candidate in candidates {
        let key = (home.to_path_buf(), candidate.id.clone());
        let stale = cache.get(&key).is_none_or(|entry| {
            entry.fingerprint != candidate.fingerprint
                || (entry.update.state != "checking"
                    && entry.checked_at.elapsed()
                        >= if entry.update.state == "unknown" {
                            RETRY_TTL
                        } else {
                            CACHE_TTL
                        })
        });
        if stale {
            let checkable = !matches!(candidate.source, Source::Unknown);
            cache.insert(
                key.clone(),
                Entry {
                    fingerprint: candidate.fingerprint.clone(),
                    checked_at: Instant::now(),
                    update: Update::new(
                        &candidate.id,
                        if checkable { "checking" } else { "unknown" },
                        candidate.installed_version.clone(),
                    ),
                },
            );
            if checkable {
                jobs.push((key.clone(), candidate));
            }
        }
        items.push(cache[&key].update.clone());
    }
    drop(cache);
    if !jobs.is_empty() {
        std::thread::spawn(move || {
            // Limit simultaneous version processes and release requests.
            for batch in jobs.chunks(3) {
                std::thread::scope(|scope| {
                    for (key, candidate) in batch {
                        scope.spawn(move || {
                            let update = std::panic::catch_unwind(|| check(candidate))
                                .unwrap_or_else(|_| {
                                    Update::new(
                                        &candidate.id,
                                        "unknown",
                                        candidate.installed_version.clone(),
                                    )
                                });
                            let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
                            if let Some(entry) = cache
                                .get_mut(key)
                                .filter(|entry| entry.fingerprint == candidate.fingerprint)
                            {
                                entry.update = update;
                                entry.checked_at = Instant::now();
                            }
                        });
                    }
                });
            }
        });
    }
    json!({ "checking": items.iter().any(|item| item.state == "checking"), "items": items })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn check(candidate: &Candidate) -> Update {
    check_with(candidate, &crate::http_fetch::get, &probe_version)
}

fn check_with(
    candidate: &Candidate,
    fetch: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    probe: &impl Fn(&Path, &[String]) -> Result<String, String>,
) -> Update {
    let mut result = Update::new(
        &candidate.id,
        "unknown",
        candidate.installed_version.clone(),
    );
    match &candidate.source {
        Source::Unknown => return result,
        Source::App {
            checksum_url,
            digest,
        } => {
            let Ok(bytes) = fetch(checksum_url) else {
                return result;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return result;
            };
            let latest = text.split_whitespace().next().unwrap_or_default();
            if !valid_digest(latest) {
                return result;
            }
            result.update_available = !latest.eq_ignore_ascii_case(digest);
        }
        Source::Agent(recipe) => {
            let Ok(installed) = probe(&candidate.binary, &recipe.version_args) else {
                return result;
            };
            let Some(installed) = version_in(&installed, recipe.date_version) else {
                return result;
            };
            result.installed_version = Some(installed.clone());
            let Ok(bytes) = fetch(&recipe.latest_url) else {
                return result;
            };
            let Some(latest) = latest_version(&bytes, recipe) else {
                return result;
            };
            let Some(newer) = is_newer(&installed, &latest, recipe.date_version) else {
                return result;
            };
            result.update_available = newer;
            result.latest_version = Some(latest);
        }
    }
    result.state = if result.update_available {
        "available"
    } else {
        "current"
    };
    result
}

fn latest_version(bytes: &[u8], recipe: &RuntimeUpdates) -> Option<String> {
    let text = if let Some(pointer) = &recipe.json_pointer {
        serde_json::from_slice::<Value>(bytes)
            .ok()?
            .pointer(pointer)?
            .as_str()?
            .to_owned()
    } else {
        std::str::from_utf8(bytes).ok()?.to_owned()
    };
    let text = if let Some(prefix) = &recipe.version_prefix {
        text.split_once(prefix)?.1
    } else {
        &text
    };
    let text = if let Some(suffix) = &recipe.version_suffix {
        text.split_once(suffix)?.0
    } else {
        text
    };
    version_in(text, recipe.date_version)
}

fn version_in(text: &str, date: bool) -> Option<String> {
    text.split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| {
                !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+')
            })
        })
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            if date {
                date_key(token).is_some()
            } else {
                semver::Version::parse(token).is_ok()
            }
        })
        .map(str::to_owned)
}

fn date_key(version: &str) -> Option<(u16, u8, u8)> {
    let mut parts = version.split('-').next()?.split('.');
    let key = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    (parts.next().is_none()
        && key.0 >= 2020
        && (1..=12).contains(&key.1)
        && (1..=31).contains(&key.2))
    .then_some(key)
}

fn is_newer(installed: &str, latest: &str, date: bool) -> Option<bool> {
    if date {
        let current = date_key(installed)?;
        let next = date_key(latest)?;
        Some(next > current || (next == current && latest != installed))
    } else {
        Some(semver::Version::parse(latest).ok()? > semver::Version::parse(installed).ok()?)
    }
}

fn probe_version(binary: &Path, args: &[String]) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(path) = std::env::join_paths(crate::setup::search_dirs()) {
        command.env("PATH", path);
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child.id();
    let started = crate::session_host::process_start_time_ms(pid);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            let error = std::io::Error::last_os_error().to_string();
            stop_probe(&mut child, started);
            return Err(error);
        }
    }
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut output = Vec::new();
    loop {
        let status = child.try_wait().map_err(|error| error.to_string())?;
        let mut buf = [0; 1024];
        for pipe in [&mut stdout as &mut dyn Read, &mut stderr as &mut dyn Read] {
            while let Ok(count) = pipe.read(&mut buf) {
                if count == 0 {
                    break;
                }
                output.extend_from_slice(&buf[..count]);
                if output.len() > MAX_VERSION_OUTPUT {
                    break;
                }
            }
        }
        if let Some(status) = status {
            return if status.success() && output.len() <= MAX_VERSION_OUTPUT {
                String::from_utf8(output).map_err(|error| error.to_string())
            } else {
                Err("version probe failed".into())
            };
        }
        if Instant::now() >= deadline || output.len() > MAX_VERSION_OUTPUT {
            stop_probe(&mut child, started);
            return Err("version probe exceeded its limit".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn stop_probe(child: &mut std::process::Child, started: Option<u64>) {
    // This detached process group belongs to this probe. Verify the
    // recorded leader identity immediately before signaling it.
    let pid = child.id();
    if started.is_some() && crate::session_host::process_start_time_ms(pid) == started {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn recipe() -> RuntimeUpdates {
        RuntimeUpdates {
            version_args: vec!["--version".into()],
            latest_url: "https://example.com/latest".into(),
            json_pointer: Some("/version".into()),
            version_prefix: None,
            version_suffix: None,
            date_version: false,
            channel_env: None,
            channel_settings_path: None,
            channel_settings_pointer: None,
            channel_urls: Default::default(),
        }
    }

    #[test]
    fn updates_require_a_confirmed_newer_release() {
        let candidate = Candidate::new(
            "agent".into(),
            "/private/agent".into(),
            None,
            Source::Agent(recipe()),
        );
        for (installed, latest, expected) in [
            ("agent 1.9.0", "1.10.0", true),
            ("1.10.0", "1.9.0", false),
            ("1.10.0", "1.10.0", false),
            ("2.0.0-beta.1", "2.0.0", true),
        ] {
            let value = check_with(
                &candidate,
                &|_| Ok(json!({"version":latest}).to_string().into_bytes()),
                &|_, _| Ok(installed.into()),
            );
            assert_eq!(value.update_available, expected, "{installed} -> {latest}");
            assert_eq!(value.state, if expected { "available" } else { "current" });
        }
        let offline = check_with(&candidate, &|_| Err("offline".into()), &|_, _| {
            Ok("1.0.0".into())
        });
        assert!(!offline.update_available);
        assert_eq!(offline.state, "unknown");
        assert_eq!(offline.installed_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn app_checks_compare_the_installed_release_digest_and_reject_invalid_responses() {
        let digest = "a".repeat(64);
        let candidate = Candidate::new(
            "app".into(),
            "/private/app".into(),
            Some("1.0.0".into()),
            Source::App {
                checksum_url: "https://example.com/latest.sha256".into(),
                digest: digest.clone(),
            },
        );
        for (body, expected, state) in [
            (format!("{digest}  app.tar.gz\n"), false, "current"),
            (
                format!("{}  app.tar.gz\n", "b".repeat(64)),
                true,
                "available",
            ),
            ("<html>not found</html>".into(), false, "unknown"),
        ] {
            let value = check_with(&candidate, &|_| Ok(body.clone().into_bytes()), &|_, _| {
                panic!("Apps must not run version processes")
            });
            assert_eq!(value.update_available, expected);
            assert_eq!(value.state, state);
        }
    }

    #[test]
    fn missing_version_and_development_links_do_not_claim_updates() {
        let calls = AtomicUsize::new(0);
        let fetch = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        };
        let candidate = Candidate::new(
            "linked".into(),
            "/private/linked".into(),
            None,
            Source::Unknown,
        );
        assert!(
            !check_with(&candidate, &fetch, &|_, _| panic!(
                "linked builds are not probed"
            ))
            .update_available
        );
        let candidate = Candidate::new(
            "unknown".into(),
            "/private/agent".into(),
            None,
            Source::Agent(recipe()),
        );
        assert!(
            !check_with(&candidate, &fetch, &|_, _| Ok("usage: agent".into())).update_available
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn settings_cache_is_scoped_to_the_host_home_and_installation() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let make = |version: &str| {
            Candidate::new(
                "same-id".into(),
                "/private/app".into(),
                Some(version.into()),
                Source::Unknown,
            )
        };
        assert_eq!(
            request_with(a.path(), vec![make("1.0.0")])["items"][0]["installedVersion"],
            "1.0.0"
        );
        assert_eq!(
            request_with(b.path(), vec![make("2.0.0")])["items"][0]["installedVersion"],
            "2.0.0"
        );
        assert_eq!(
            request_with(a.path(), vec![make("3.0.0")])["items"][0]["installedVersion"],
            "3.0.0"
        );
        assert_eq!(
            request_with(b.path(), vec![make("2.0.0")])["items"][0]["installedVersion"],
            "2.0.0"
        );
    }

    #[test]
    fn dated_versions_use_release_order_and_accept_installer_metadata() {
        let mut recipe = recipe();
        recipe.date_version = true;
        recipe.json_pointer = None;
        recipe.version_prefix = Some("https://example.com/releases/".into());
        recipe.version_suffix = Some("/".into());
        let latest = latest_version(
            b"DOWNLOAD_URL=\"https://example.com/releases/2026.09.02-abcd/linux/archive\"",
            &recipe,
        )
        .unwrap();
        assert_eq!(latest, "2026.09.02-abcd");
        assert_eq!(is_newer("2026.08.28-xxxx", &latest, true), Some(true));
        assert_eq!(is_newer("2026.09.03-xxxx", &latest, true), Some(false));
    }

    #[test]
    fn version_probe_captures_stdout_and_stderr_without_a_terminal() {
        let output = probe_version(
            Path::new("/bin/sh"),
            &["-c".into(), "printf 'agent '; printf '1.2.3' >&2".into()],
        )
        .unwrap();
        assert_eq!(version_in(&output, false).as_deref(), Some("1.2.3"));
    }
}
