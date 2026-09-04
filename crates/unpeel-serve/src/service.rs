//! Machine-wide Unpeel Host service.
//!
//! `HostRuntime` remains deliberately scoped to one `UNPEEL_HOME`: a large
//! part of the released on-disk contract resolves paths process-wide, and
//! putting two homes in one address space would let one workspace leak into
//! another. The service is the stable user-facing lifecycle above that
//! boundary. It owns one machine lease and supervises one small Host worker
//! process per workspace, including the implicit default workspace.
//!
//! This is still one logical service to the app, CLI, and service managers.
//! Worker processes are an isolation detail, just as every Session already
//! has its own persistent `unpeel-host` process.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::driver::{self, ServeEvent};

pub const SERVICE_ARG: &str = "__serve__";
pub const WORKSPACE_WORKER_ARG: &str = "__serve_workspace__";

const SERVICE_STATUS_VERSION: u64 = 1;
const LOOP_INTERVAL: Duration = Duration::from_millis(100);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const RESTART_DELAY: Duration = Duration::from_secs(2);
const EXTERNAL_RECHECK_DELAY: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(5);

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_shutdown(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceEvent {
    Started {
        pid: u32,
        workspace_count: usize,
    },
    WorkspaceStarted {
        name: String,
        home: PathBuf,
        pid: u32,
    },
    WorkspaceExternal {
        name: String,
        home: PathBuf,
    },
    WorkspaceStopped {
        name: String,
        home: PathBuf,
    },
    Warning(String),
    Stopped,
}

impl fmt::Display for ServiceEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Started {
                pid,
                workspace_count,
            } => write!(
                formatter,
                "Unpeel Host service started (pid {pid}, {workspace_count} workspaces)"
            ),
            Self::WorkspaceStarted { name, home, pid } => write!(
                formatter,
                "Host workspace {name:?} started (pid {pid}, {})",
                home.display()
            ),
            Self::WorkspaceExternal { name, home } => write!(
                formatter,
                "Host workspace {name:?} is already served ({})",
                home.display()
            ),
            Self::WorkspaceStopped { name, home } => write!(
                formatter,
                "Host workspace {name:?} stopped ({})",
                home.display()
            ),
            Self::Warning(message) => write!(formatter, "warning: {message}"),
            Self::Stopped => formatter.write_str("Unpeel Host service stopped"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostServiceEvent {
    Service(ServiceEvent),
    Workspace(ServeEvent),
}

impl fmt::Display for HostServiceEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(event) => event.fmt(formatter),
            Self::Workspace(event) => event.fmt(formatter),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceTarget {
    id: String,
    name: String,
    home: PathBuf,
    is_default: bool,
}

#[derive(Deserialize, Default)]
struct WorkspaceRegistry {
    #[serde(default, rename = "profiles")]
    workspaces: Vec<WorkspaceRecord>,
}

#[derive(Deserialize)]
struct WorkspaceRecord {
    id: String,
    name: String,
    home: PathBuf,
}

fn normalized_home(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn workspace_targets(real_home: &Path) -> Vec<WorkspaceTarget> {
    let _ = std::fs::create_dir_all(real_home);
    let default_home = normalized_home(real_home);
    let mut targets = vec![WorkspaceTarget {
        id: "default".into(),
        name: "Default".into(),
        home: real_home.to_path_buf(),
        is_default: true,
    }];
    let registry = std::fs::read(real_home.join("profiles.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<WorkspaceRegistry>(&raw).ok())
        .unwrap_or_default();
    let mut seen = HashSet::from([default_home]);
    for record in registry.workspaces {
        if record.id.trim().is_empty()
            || record.name.trim().is_empty()
            || !record.home.is_absolute()
        {
            continue;
        }
        let _ = std::fs::create_dir_all(&record.home);
        let normalized = normalized_home(&record.home);
        if !seen.insert(normalized) {
            continue;
        }
        targets.push(WorkspaceTarget {
            id: record.id,
            name: record.name,
            home: record.home,
            is_default: false,
        });
    }
    targets
}

struct ServiceLease {
    _file: std::fs::File,
    status_path: PathBuf,
    pid: u32,
}

impl ServiceLease {
    fn acquire(real_home: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(real_home)
            .map_err(|error| format!("could not create {}: {error}", real_home.display()))?;
        let lock_path = real_home.join("host-service.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&lock_path)
            .map_err(|error| format!("could not open {}: {error}", lock_path.display()))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err("the Unpeel Host service is already running".into());
            }
            return Err(format!("could not lock {}: {error}", lock_path.display()));
        }
        Ok(Self {
            _file: file,
            status_path: real_home.join("host-service.json"),
            pid: std::process::id(),
        })
    }

    fn publish(&self, status: &ServiceStatus) -> Result<(), String> {
        let body = serde_json::to_vec_pretty(status).map_err(|error| error.to_string())?;
        let temporary = self.status_path.with_file_name(format!(
            ".host-service.{}.{}.tmp",
            self.pid,
            uuid::Uuid::new_v4()
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&body)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.status_path)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!(
                "could not publish {}: {error}",
                self.status_path.display()
            ));
        }
        Ok(())
    }
}

impl Drop for ServiceLease {
    fn drop(&mut self) {
        let belongs_to_us = std::fs::read(&self.status_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .and_then(|value| value.get("pid").and_then(serde_json::Value::as_u64))
            == Some(u64::from(self.pid));
        if belongs_to_us {
            let _ = std::fs::remove_file(&self.status_path);
        }
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatus {
    version: u64,
    pid: u32,
    started_at_unix_ms: u64,
    executable: PathBuf,
    /// Additive (0.4.0): the supervising binary's version and build stamp,
    /// so an app bundled with a different `unpeel-host` can restart a stale
    /// service after an in-place update instead of driving it.
    host_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_id: Option<String>,
    workspaces: Vec<WorkspaceStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceStatus {
    id: String,
    name: String,
    home: PathBuf,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    serve: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

struct ManagedWorker {
    target: WorkspaceTarget,
    child: Option<Child>,
    restart_at: Instant,
    last_error: Option<String>,
    external_reported: bool,
}

impl ManagedWorker {
    fn new(target: WorkspaceTarget) -> Self {
        Self {
            target,
            child: None,
            restart_at: Instant::now(),
            last_error: None,
            external_reported: false,
        }
    }

    fn owned_pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    fn serve_status(&self) -> Option<serde_json::Value> {
        std::fs::read(self.target.home.join("serve.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
    }

    fn status(&self) -> WorkspaceStatus {
        let serve = self.serve_status();
        let external = self.child.is_none() && driver::is_running_at(&self.target.home);
        let pid = self.owned_pid().or_else(|| {
            serve
                .as_ref()
                .and_then(|value| value.get("pid"))
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
        });
        WorkspaceStatus {
            id: self.target.id.clone(),
            name: self.target.name.clone(),
            home: self.target.home.clone(),
            state: if self.child.is_some() {
                "running"
            } else if external {
                "external"
            } else {
                "starting"
            },
            pid,
            serve,
            last_error: self.last_error.clone(),
        }
    }

    fn poll(&mut self, executable: &Path, report: &mut impl FnMut(ServiceEvent)) -> bool {
        let mut changed = false;
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    self.last_error = Some(format!("workspace Host exited with {status}"));
                    self.restart_at = Instant::now() + RESTART_DELAY;
                    report(ServiceEvent::WorkspaceStopped {
                        name: self.target.name.clone(),
                        home: self.target.home.clone(),
                    });
                    changed = true;
                }
                Ok(None) => return changed,
                Err(error) => {
                    self.child = None;
                    self.last_error = Some(format!("could not inspect workspace Host: {error}"));
                    self.restart_at = Instant::now() + RESTART_DELAY;
                    changed = true;
                }
            }
        }
        if self.child.is_some() || Instant::now() < self.restart_at {
            return changed;
        }
        if driver::is_running_at(&self.target.home) {
            if !self.external_reported {
                report(ServiceEvent::WorkspaceExternal {
                    name: self.target.name.clone(),
                    home: self.target.home.clone(),
                });
                self.external_reported = true;
                changed = true;
            }
            self.restart_at = Instant::now() + EXTERNAL_RECHECK_DELAY;
            return changed;
        }
        self.external_reported = false;
        match spawn_workspace(executable, &self.target) {
            Ok(child) => {
                let pid = child.id();
                self.child = Some(child);
                self.last_error = None;
                report(ServiceEvent::WorkspaceStarted {
                    name: self.target.name.clone(),
                    home: self.target.home.clone(),
                    pid,
                });
            }
            Err(error) => {
                self.last_error = Some(error.clone());
                self.restart_at = Instant::now() + RESTART_DELAY;
                report(ServiceEvent::Warning(format!(
                    "could not start workspace {:?}: {error}",
                    self.target.name
                )));
            }
        }
        true
    }

    fn stop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let pid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                self.child = None;
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        self.child = None;
    }
}

fn spawn_workspace(executable: &Path, target: &WorkspaceTarget) -> Result<Child, String> {
    std::fs::create_dir_all(&target.home)
        .map_err(|error| format!("prepare {}: {error}", target.home.display()))?;
    let mut command = Command::new(executable);
    command
        .arg(WORKSPACE_WORKER_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if target.is_default {
        command.env_remove("UNPEEL_HOME");
    } else {
        command.env("UNPEEL_HOME", &target.home);
    }
    command
        .spawn()
        .map_err(|error| format!("launch {}: {error}", executable.display()))
}

fn machine_service_is_running_at(real_home: &Path) -> bool {
    let path = real_home.join("host-service.lock");
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
    else {
        return false;
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
    }
}

pub fn is_running() -> bool {
    machine_service_is_running_at(&unpeel_core::app_paths::real_unpeel_home())
}

fn publish_status(
    lease: &ServiceLease,
    executable: &Path,
    started_at_unix_ms: u64,
    workers: &HashMap<PathBuf, ManagedWorker>,
) -> Result<(), String> {
    let mut workspaces = workers
        .values()
        .map(ManagedWorker::status)
        .collect::<Vec<_>>();
    workspaces.sort_by(|left, right| left.name.cmp(&right.name));
    lease.publish(&ServiceStatus {
        version: SERVICE_STATUS_VERSION,
        pid: std::process::id(),
        started_at_unix_ms,
        executable: executable.to_path_buf(),
        host_version: env!("CARGO_PKG_VERSION"),
        build_id: unpeel_core::session_host::current_host_build_id(),
        workspaces,
    })
}

/// Run the machine-wide supervisor in the foreground.
pub fn run_service(mut report: impl FnMut(ServiceEvent)) -> Result<(), String> {
    SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    unsafe {
        libc::signal(libc::SIGINT, request_shutdown as libc::sighandler_t);
        libc::signal(libc::SIGTERM, request_shutdown as libc::sighandler_t);
    }
    let real_home = unpeel_core::app_paths::real_unpeel_home();
    let lease = ServiceLease::acquire(&real_home)?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not resolve Host executable: {error}"))?;
    let started_at_unix_ms = now_ms();
    let mut workers = HashMap::<PathBuf, ManagedWorker>::new();
    let mut last_reconcile = Instant::now() - RECONCILE_INTERVAL;
    let initial = workspace_targets(&real_home);
    report(ServiceEvent::Started {
        pid: std::process::id(),
        workspace_count: initial.len(),
    });
    reconcile_workers(initial, &mut workers);
    let mut status_dirty = true;
    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        if last_reconcile.elapsed() >= RECONCILE_INTERVAL {
            reconcile_workers(workspace_targets(&real_home), &mut workers);
            last_reconcile = Instant::now();
            status_dirty = true;
        }
        for worker in workers.values_mut() {
            status_dirty |= worker.poll(&executable, &mut report);
        }
        if status_dirty {
            if let Err(error) = publish_status(&lease, &executable, started_at_unix_ms, &workers) {
                report(ServiceEvent::Warning(error));
            }
            status_dirty = false;
        }
        std::thread::sleep(LOOP_INTERVAL);
    }
    for worker in workers.values_mut() {
        worker.stop();
    }
    drop(lease);
    report(ServiceEvent::Stopped);
    Ok(())
}

fn reconcile_workers(targets: Vec<WorkspaceTarget>, workers: &mut HashMap<PathBuf, ManagedWorker>) {
    let desired = targets
        .into_iter()
        .map(|target| (normalized_home(&target.home), target))
        .collect::<HashMap<_, _>>();
    let removed = workers
        .keys()
        .filter(|key| !desired.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    for key in removed {
        if let Some(mut worker) = workers.remove(&key) {
            worker.stop();
        }
    }
    for (key, target) in desired {
        match workers.get_mut(&key) {
            Some(worker) if worker.target == target => {}
            Some(worker) => {
                worker.stop();
                *worker = ManagedWorker::new(target);
            }
            None => {
                workers.insert(key, ManagedWorker::new(target));
            }
        }
    }
}

/// Public command behavior. An explicit/scoped `UNPEEL_HOME` remains a
/// one-workspace foreground Host (useful for containers and service-manager
/// units); the ordinary unscoped command is the machine-wide service.
pub fn run(mut report: impl FnMut(HostServiceEvent)) -> Result<(), String> {
    if std::env::var_os("UNPEEL_HOME").is_some_and(|value| !value.is_empty()) {
        driver::run(|event| {
            crate::tracelog::trace("host-worker", &event.to_string());
            report(HostServiceEvent::Workspace(event));
        })
    } else {
        run_service(|event| {
            crate::tracelog::trace("host-service", &event.to_string());
            report(HostServiceEvent::Service(event));
        })
    }
}

/// Internal worker entry point shared by the `unpeel` and `unpeel-host`
/// binaries. It is intentionally not a user-facing command.
pub fn run_workspace_worker(mut report: impl FnMut(ServeEvent)) -> Result<(), String> {
    driver::run(|event| {
        crate::tracelog::trace("host-worker", &event.to_string());
        report(event);
    })
}

/// Start the appropriate Host lifecycle detached from a frontend. Races are
/// harmless: leases choose one winner and every loser exits immediately.
pub fn ensure_background(executable: &Path) -> Result<(), String> {
    let scoped = std::env::var_os("UNPEEL_HOME").is_some_and(|value| !value.is_empty());
    if (scoped && driver::is_running()) || (!scoped && is_running()) {
        return Ok(());
    }
    let mut command = Command::new(executable);
    command
        .arg(SERVICE_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("start the Unpeel Host service: {error}"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_targets_include_default_and_deduplicate_homes() {
        let root = tempfile::tempdir().unwrap();
        let named = root.path().join("profiles/writing");
        std::fs::create_dir_all(&named).unwrap();
        let registry = serde_json::json!({
            "version": 1,
            "profiles": [
                {"id":"writing","name":"Writing","home":named,"createdAt":1},
                {"id":"duplicate","name":"Duplicate","home":named,"createdAt":2},
                {"id":"relative","name":"Relative","home":"profiles/nope","createdAt":3}
            ]
        });
        std::fs::write(
            root.path().join("profiles.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();
        let targets = workspace_targets(root.path());
        assert_eq!(targets.len(), 2);
        assert!(targets[0].is_default);
        assert_eq!(targets[1].name, "Writing");
    }
}
