//! Git settings and GitHub CLI device authorization on the selected Host.
//! This support adapter is not an agent runtime and adds no launch preset.
//! `gh` owns credentials in its normal credential store. Only account metadata
//! and a short-lived device challenge cross the Controller protocol.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROBE_TIMEOUT: Duration = Duration::from_secs(12);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    checking: bool,
    git_installed: bool,
    github_cli_installed: bool,
    github_cli_install_command: Option<String>,
    name: String,
    email: String,
    github: Account,
    operation: Option<Operation>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    state: String,
    login: Option<String>,
    environment_token: bool,
    credential_storage: &'static str,
    git_configured: bool,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            state: "notConnected".into(),
            login: None,
            environment_token: false,
            credential_storage: "unknown",
            git_configured: false,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    id: String,
    action: String,
    state: String,
    user_code: Option<String>,
    verification_url: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    pub action: String,
    #[serde(rename = "requestID")]
    pub request_id: String,
    #[serde(default, rename = "operationID")]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl Action {
    pub fn validate(&self) -> Result<(), &'static str> {
        if uuid::Uuid::parse_str(&self.request_id).is_err() {
            return Err("A requestID UUID is required.");
        }
        match self.action.as_str() {
            "connectGithub" | "setupGit" | "refresh" => {}
            "cancelGithub"
                if self
                    .operation_id
                    .as_ref()
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()) => {}
            "disconnectGithub" if self.login.as_ref().is_some_and(|login| valid_login(login)) => {}
            "setIdentity"
                if self
                    .name
                    .as_ref()
                    .is_some_and(|value| valid_identity(value))
                    && self
                        .email
                        .as_ref()
                        .is_some_and(|value| valid_identity(value)) => {}
            _ => return Err("Invalid Git settings action or fields."),
        }
        Ok(())
    }
}

fn valid_identity(value: &str) -> bool {
    value.len() <= 254 && !value.chars().any(char::is_control)
}

struct Entry {
    settings: Settings,
    busy: bool,
    checked: Option<Instant>,
    cancelled: Arc<AtomicBool>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            busy: false,
            checked: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
}

static ENTRIES: LazyLock<Mutex<HashMap<PathBuf, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn entries() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Entry>> {
    ENTRIES.lock().unwrap_or_else(|error| error.into_inner())
}

#[derive(Clone)]
struct Context {
    workspace: PathBuf,
    machine: PathBuf,
    user_home: PathBuf,
    git: Option<PathBuf>,
    gh: Option<PathBuf>,
    search_path: std::ffi::OsString,
}

impl Context {
    fn current() -> Self {
        let dirs = crate::setup::search_dirs();
        Self {
            workspace: crate::app_paths::unpeel_home(),
            machine: crate::app_paths::machine_home(),
            user_home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            git: crate::setup::find_command_path("git", &dirs).map(PathBuf::from),
            gh: crate::setup::find_command_path("gh", &dirs).map(PathBuf::from),
            search_path: std::env::join_paths(dirs).unwrap_or_default(),
        }
    }

    fn command(&self, binary: &Option<PathBuf>, args: &[&str]) -> Result<Command, &'static str> {
        let mut command = Command::new(
            binary
                .as_ref()
                .ok_or("Install the required CLI on this Host first.")?,
        );
        command
            .args(args)
            .current_dir(&self.user_home)
            .env("PATH", &self.search_path)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_BROWSER", "/usr/bin/false")
            .env("BROWSER", "/usr/bin/false")
            .env("GH_PAGER", "cat")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("LC_ALL", "C")
            .env_remove("GH_FORCE_TTY")
            .env_remove("GH_DEBUG");
        Ok(command)
    }

    fn run(
        &self,
        binary: &Option<PathBuf>,
        args: &[&str],
        cancelled: &AtomicBool,
    ) -> Result<crate::host_command::Output, &'static str> {
        crate::host_command::run(
            &mut self.command(binary, args)?,
            PROBE_TIMEOUT,
            cancelled,
            |_| {},
        )
    }
}

pub fn read() -> Value {
    let context = Context::current();
    let mut entries = entries();
    let entry = entries.entry(context.workspace.clone()).or_default();
    if !entry.busy
        && entry
            .checked
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(60))
    {
        entry.busy = true;
        entry.settings.checking = true;
        let cancelled = Arc::new(AtomicBool::new(false));
        entry.cancelled = cancelled.clone();
        std::thread::spawn(move || finish(context, None, cancelled, None));
    }
    serde_json::to_value(&entry.settings).expect("Git settings serialize")
}

pub fn write(body: &Value) -> (u16, Value) {
    let request: Action = match serde_json::from_value(body.clone()) {
        Ok(request) => request,
        Err(_) => return (400, json!({"error":"Invalid Git settings request."})),
    };
    if let Err(error) = request.validate() {
        return (400, json!({"error":error}));
    }
    start(Context::current(), request)
}

fn start(context: Context, request: Action) -> (u16, Value) {
    let mut entries = entries();
    let entry = entries.entry(context.workspace.clone()).or_default();
    if request.action == "cancelGithub" {
        if entry.busy
            && entry.settings.operation.as_ref().is_some_and(|op| {
                op.action == "connectGithub" && Some(&op.id) == request.operation_id.as_ref()
            })
        {
            entry.cancelled.store(true, Ordering::Release);
        }
        return (200, json!({"ok":true}));
    }
    if entry
        .settings
        .operation
        .as_ref()
        .is_some_and(|op| op.id == request.request_id)
    {
        return (200, json!({"ok":true}));
    }
    if entry.busy {
        return (
            409,
            json!({"error":"A Git operation is already running. Try again when it finishes."}),
        );
    }
    // All workspaces of one OS account use gh's ordinary configuration. Keep
    // explicit writes serialized across workspace worker processes as well.
    let lock = match mutation_lock(&context.machine) {
        Ok(lock) => lock,
        Err(error) => return (409, json!({"error":error})),
    };
    entry.busy = true;
    entry.cancelled = Arc::new(AtomicBool::new(false));
    entry.settings.error = None;
    entry.settings.operation = Some(Operation {
        id: request.request_id.clone(),
        action: request.action.clone(),
        state: "running".into(),
        user_code: None,
        verification_url: None,
        error: None,
    });
    let cancelled = entry.cancelled.clone();
    std::thread::spawn(move || finish(context, Some(request), cancelled, Some(lock)));
    (200, json!({"ok":true}))
}

fn mutation_lock(home: &std::path::Path) -> Result<File, &'static str> {
    std::fs::create_dir_all(home).map_err(|_| "Could not prepare Git setup on this Host.")?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("git-settings.lock"))
        .map_err(|_| "Could not lock Git setup on this Host.")?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Git setup is running in another workspace on this Host.");
    }
    Ok(file)
}

fn finish(
    context: Context,
    request: Option<Action>,
    cancelled: Arc<AtomicBool>,
    _lock: Option<File>,
) {
    let result = request
        .as_ref()
        .map(|request| execute(&context, request, &cancelled))
        .unwrap_or(Ok(()));
    // Always recheck after cancellation: authorization may have completed just
    // before it arrived. Cancelling the UI is never a claim of revocation.
    let mut settings = inspect(&context, &AtomicBool::new(false));
    let mut entries = entries();
    let entry = entries.entry(context.workspace.clone()).or_default();
    settings.operation = entry.settings.operation.clone();
    if request.is_some() {
        if let Some(operation) = &mut settings.operation {
            operation.state = if result.is_ok() {
                "complete"
            } else if cancelled.load(Ordering::Acquire) {
                "cancelled"
            } else {
                "failed"
            }
            .into();
            operation.error = result
                .err()
                .filter(|error| *error != "Cancelled")
                .map(str::to_owned);
            operation.user_code = None;
            operation.verification_url = None;
        }
        crate::state_bus::announce(crate::state_bus::Change::AppState, None);
    }
    entry.settings = settings;
    entry.busy = false;
    entry.checked = Some(Instant::now());
}

fn execute(
    context: &Context,
    request: &Action,
    cancelled: &AtomicBool,
) -> Result<(), &'static str> {
    match request.action.as_str() {
        "refresh" => Ok(()),
        "connectGithub" => {
            let mut command = context.command(
                &context.gh,
                &[
                    "auth",
                    "login",
                    "--hostname",
                    "github.com",
                    "--git-protocol",
                    "https",
                    "--web",
                ],
            )?;
            let output =
                crate::host_command::run(&mut command, LOGIN_TIMEOUT, cancelled, |stderr| {
                    if let Some(code) = device_code(stderr) {
                        let mut entries = entries();
                        if let Some(operation) = entries
                            .get_mut(&context.workspace)
                            .and_then(|entry| entry.settings.operation.as_mut())
                        {
                            if operation.id == request.request_id {
                                operation.state = "awaitingAuthorization".into();
                                operation.user_code = Some(code);
                                operation.verification_url =
                                    Some("https://github.com/login/device".into());
                            }
                        }
                    }
                })?;
            if !output.success {
                return Err("GitHub sign-in did not complete. Check the Host’s network connection and try again. If GH_TOKEN is set, manage that token on the Host.");
            }
            setup_git(context, cancelled)
        }
        "setupGit" => setup_git(context, cancelled),
        "disconnectGithub" => {
            let output = context.run(
                &context.gh,
                &[
                    "auth",
                    "logout",
                    "--hostname",
                    "github.com",
                    "--user",
                    request.login.as_deref().unwrap_or_default(),
                ],
                cancelled,
            )?;
            if output.success {
                Ok(())
            } else {
                Err("Could not disconnect this GitHub account. Environment tokens must be removed on the Host.")
            }
        }
        "setIdentity" => {
            for (key, value) in [
                ("user.name", request.name.as_deref().unwrap_or_default()),
                ("user.email", request.email.as_deref().unwrap_or_default()),
            ] {
                let output = context.run(
                    &context.git,
                    &["config", "--global", "--replace-all", key, value.trim()],
                    cancelled,
                )?;
                if !output.success {
                    return Err("Could not save Git identity on this Host. Refresh to check the current values.");
                }
            }
            Ok(())
        }
        _ => Err("Unsupported Git action."),
    }
}

fn setup_git(context: &Context, cancelled: &AtomicBool) -> Result<(), &'static str> {
    let output = context.run(
        &context.gh,
        &["auth", "setup-git", "--hostname", "github.com"],
        cancelled,
    )?;
    if output.success {
        Ok(())
    } else {
        Err("GitHub is signed in, but Git setup failed. Use Configure Git to retry.")
    }
}

fn inspect(context: &Context, cancelled: &AtomicBool) -> Settings {
    let mut settings = Settings {
        git_installed: context.git.is_some(),
        github_cli_installed: context.gh.is_some(),
        ..Settings::default()
    };
    let dirs: Vec<_> = std::env::split_paths(&context.search_path).collect();
    settings.github_cli_install_command = [
        ("brew", "brew install gh git"),
        (
            "apt-get",
            "sudo apt-get update && sudo apt-get install -y gh git",
        ),
        ("dnf", "sudo dnf install -y gh git"),
    ]
    .into_iter()
    .find(|(binary, _)| crate::setup::find_command_path(binary, &dirs).is_some())
    .map(|(_, command)| command.to_owned());
    for (key, target) in [
        ("user.name", &mut settings.name),
        ("user.email", &mut settings.email),
    ] {
        if let Ok(output) = context.run(
            &context.git,
            &["config", "--global", "--get", key],
            cancelled,
        ) {
            if output.success {
                *target = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .chars()
                    .take(254)
                    .collect();
            }
        }
    }
    if context.gh.is_none() {
        settings.github.state = "unavailable".into();
        return settings;
    }
    match context.run(
        &context.gh,
        &[
            "auth",
            "status",
            "--hostname",
            "github.com",
            "--active",
            "--json",
            "hosts",
        ],
        cancelled,
    ) {
        Ok(output) => {
            match parse_account(&output.stdout).or_else(|| legacy_account(context, cancelled)) {
                Some(account) => settings.github = account,
                None => {
                    settings.github.state = "error".into();
                    settings.error = Some("Could not check GitHub. Check the Host’s network connection and GitHub CLI version, then refresh.".into());
                }
            }
        }
        Err(error) => {
            settings.github.state = "error".into();
            settings.error = Some(error.into());
        }
    }
    if let Ok(output) = context.run(
        &context.git,
        &[
            "config",
            "--global",
            "--get-all",
            "credential.https://github.com.helper",
        ],
        cancelled,
    ) {
        settings.github.git_configured = output.success
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.contains("auth git-credential"));
    }
    settings
}

/// Distribution packages can predate `auth status --json`. The long-standing
/// `api user` command still verifies the active account without printing its
/// credential. Never infer "signed out" from a network or API failure.
fn legacy_account(context: &Context, cancelled: &AtomicBool) -> Option<Account> {
    let mut missing_login = false;
    let mut command = context
        .command(&context.gh, &["api", "--hostname", "github.com", "user"])
        .ok()?;
    let output = crate::host_command::run(&mut command, PROBE_TIMEOUT, cancelled, |stderr| {
        missing_login =
            stderr.contains("To get started with GitHub CLI") && stderr.contains("gh auth login");
    })
    .ok()?;
    if !output.success {
        return missing_login.then(Account::default);
    }
    let body: Value = serde_json::from_slice(&output.stdout).ok()?;
    let login = body["login"].as_str().filter(|login| valid_login(login))?;
    let environment_token = ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()));
    Some(Account {
        state: "connected".into(),
        login: Some(login.into()),
        environment_token,
        credential_storage: if environment_token {
            "environment"
        } else {
            "unknown"
        },
        git_configured: false,
    })
}

fn parse_account(bytes: &[u8]) -> Option<Account> {
    let body: Value = serde_json::from_slice(bytes).ok()?;
    let hosts = body.get("hosts")?.as_object()?;
    let Some(accounts) = hosts.get("github.com") else {
        return Some(Account::default());
    };
    let active = accounts
        .as_array()?
        .iter()
        .find(|account| account["active"].as_bool() == Some(true));
    let Some(active) = active else {
        return Some(Account::default());
    };
    let login = active["login"]
        .as_str()
        .filter(|login| valid_login(login))
        .map(str::to_owned);
    Some(Account {
        state: if active["state"] == "success" && login.is_some() {
            "connected"
        } else {
            "error"
        }
        .into(),
        login,
        environment_token: active["tokenSource"]
            .as_str()
            .is_some_and(|source| matches!(source, "GH_TOKEN" | "GITHUB_TOKEN")),
        // Publish a category, never a raw path or arbitrary CLI output.
        credential_storage: match active["tokenSource"].as_str() {
            Some("GH_TOKEN" | "GITHUB_TOKEN") => "environment",
            Some("keyring") => "keyring",
            Some(source)
                if std::path::Path::new(source)
                    .file_name()
                    .is_some_and(|name| name == "hosts.yml") =>
            {
                "file"
            }
            _ => "unknown",
        },
        git_configured: false,
    })
}

fn valid_login(login: &str) -> bool {
    login
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && login.len() <= 100
        && login
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn device_code(stderr: &str) -> Option<String> {
    stderr.lines().find_map(|line| {
        let code = line.split_once("First copy your one-time code: ")?.1.trim();
        (code.len() == 9
            && code.as_bytes()[4] == b'-'
            && code.bytes().enumerate().all(|(index, byte)| {
                index == 4 || byte.is_ascii_uppercase() || byte.is_ascii_digit()
            }))
        .then(|| code.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_challenge_is_strict_and_never_forwards_arbitrary_urls_or_output() {
        assert_eq!(
            device_code(
                "! First copy your one-time code: ABCD-1234\nOpen this URL: https://evil.example"
            ),
            Some("ABCD-1234".into())
        );
        for text in [
            "secret: gho_123",
            "First copy your one-time code: bad-token",
            "First copy your one-time code: ABCD-1234 extra",
        ] {
            assert!(device_code(text).is_none());
        }
    }

    #[test]
    fn account_projection_omits_secrets_and_selects_only_the_active_account() {
        let account = parse_account(br#"{"hosts":{"github.com":[{"active":false,"login":"old","state":"success"},{"active":true,"login":"current","state":"success","tokenSource":"GH_TOKEN","token":"secret"}]}}"#).unwrap();
        let wire = serde_json::to_value(account).unwrap();
        assert_eq!(wire["login"], "current");
        assert_eq!(wire["state"], "connected");
        assert_eq!(wire["environmentToken"], true);
        assert_eq!(wire["credentialStorage"], "environment");
        assert!(!wire.to_string().contains("secret"));
        assert!(parse_account(b"not json").is_none());
        assert_eq!(
            parse_account(br#"{"hosts":{}}"#).unwrap().state,
            "notConnected"
        );
    }

    #[test]
    fn credential_storage_is_categorized_without_exposing_paths_or_claiming_unknown_is_secure() {
        for (source, expected) in [
            ("keyring", "keyring"),
            ("/private/user/.config/gh/hosts.yml", "file"),
            ("unexpected-secret", "unknown"),
        ] {
            let value = json!({"hosts":{"github.com":[{"active":true,"login":"user","state":"success","tokenSource":source}]}});
            let wire =
                serde_json::to_value(parse_account(&serde_json::to_vec(&value).unwrap()).unwrap())
                    .unwrap();
            assert_eq!(wire["credentialStorage"], expected);
            if source != "keyring" {
                assert!(!wire.to_string().contains(source));
            }
        }
    }

    #[test]
    fn invalid_requests_do_not_touch_host_configuration() {
        for body in [
            json!({}),
            json!({"action":"connectGithub","requestID":"bad"}),
            json!({"action":"setIdentity","requestID":uuid::Uuid::new_v4().to_string(),"name":"bad\nname","email":"x"}),
            json!({"action":"disconnectGithub","requestID":uuid::Uuid::new_v4().to_string(),"login":"--all"}),
        ] {
            assert_eq!(write(&body).0, 400);
        }
    }

    #[test]
    fn mutation_lock_serializes_workspaces_without_touching_real_home() {
        let home = tempfile::tempdir().unwrap();
        let lock = mutation_lock(home.path()).unwrap();
        assert!(mutation_lock(home.path()).is_err());
        drop(lock);
        // Another test in this binary may fork while the lock fd is open;
        // the child shares that flock until it execs (O_CLOEXEC), so allow
        // that brief window instead of probing exactly once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while mutation_lock(home.path()).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "lock still held after it was dropped"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
