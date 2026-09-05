//! Headless `unpeel` command surface — the scriptable half of the product.
//! Every verb here runs against the shared on-disk contract and
//! `unpeel_core::session_ops`, so agents, CI, and cron drive sessions without
//! any UI (and without the desktop app). `--json` everywhere that returns
//! data; exit codes are meaningful (`wait` returns 1 on timeout).

use std::io::Write;
use std::time::{Duration, Instant};

use unpeel_core::app_paths;

use unpeel_serve::activity::ActivityEngine;
use unpeel_serve::sessions::{scan_sidebar, ScanCache, SessionRow, SidebarItem, Status};

const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(10);
const PAIR_STANDALONE_NOTICE: &str =
    "serve isn't running; starting the Unpeel Host in the background so this box stays reachable";
const PAIR_USAGE: &str = "usage: unpeel pair [--advertise-host H] [--advertise-port P]
       unpeel pair list [--json]
       unpeel pair remove <device-id|name>
       unpeel pair relay <device-id|name> on|off

Pairing always goes through the Host service: a live `unpeel serve` owns the
window, otherwise one is started in the background first. `--serve` is
accepted for older scripts and changes nothing.";

pub const USAGE: &str = "\
unpeel — run and steer CLI agent sessions

  unpeel serve                    run the UI-free Host service for all workspaces
  unpeel serve install|uninstall|status
                                  manage the per-user boot service unit
  unpeel pair [--advertise-host H] [--advertise-port P]
                                  pair a Controller (phone, Mac app) with this Host
  unpeel pair list|remove <device>|relay <device> on|off
  unpeel --workspace NAME [...]   run any command in an isolated workspace
  unpeel ls [--json]              list sessions (status, project, command)
  unpeel new [--preset L | --command C] [--cwd D] [--json]
  unpeel send <id> <text...> [--enter]
  unpeel keys <id> <sequence>     send raw bytes (\\r, \\t, \\e escapes)
  unpeel screen <id> [--cols N] [--rows N]
  unpeel logs <id> [--lines N] [--follow]
  unpeel wait <id> [--idle] [--text S] [--timeout SECONDS]
  unpeel resume <id>               returned agent: resume in place; stopped: resume terminal
  unpeel stop|archive|restore|rm <id>
  unpeel transcript <id> [--entries N] [--markdown]
  unpeel open <path|resource> [--with APP] [--kind KIND] [--json]
  unpeel settings list|get <key>|set <key> <value> [--json]
  unpeel apps list|install <app-id> [--check] [--json]
                                  MCP gates apply to Sessions launched afterward
  unpeel presets [list | add <label> <command> | remove <label>]
  unpeel presets star|unstar|enable|disable <label|id>
  unpeel presets edit <label|id> [--label L] [--command C]
  unpeel presets reorder <label|id> <position>
  unpeel link enroll <key>        activate Unpeel Link on this Host machine
  unpeel link status [--json]     show Link enrollment and entitlement state
  unpeel link deactivate          stop Link on this Host machine
  unpeel browser install [--check] [--json]
                                  install the Host-owned browser engine
  unpeel computer install [--check] [--json]
                                  install the Host-owned computer-use engine
  unpeel workspaces [list | add <name> | remove <name>]
  unpeel add [PATH] [--name N] [--here] [--json]
                                  add a folder (default: here) as a project
  unpeel projects [list | add <name> <path> | remove <name|path>]
  unpeel hosts prune [--json]    reap leftover hosts of filed sessions
  unpeel help
  unpeel --version
";

pub const BARE_HINT: &str =
    "Run `unpeel serve` on a host, or open the Unpeel app — this binary has no terminal UI.";

struct Args {
    positional: Vec<String>,
    flags: std::collections::HashMap<String, Option<String>>,
}

fn parse(args: &[String]) -> Args {
    let mut positional = Vec::new();
    let mut flags = std::collections::HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(name) = arg.strip_prefix("--") {
            let takes_value = !matches!(
                name,
                "json" | "enter" | "follow" | "idle" | "markdown" | "all" | "here" | "serve"
            );
            if takes_value {
                flags.insert(name.to_string(), args.get(i + 1).cloned());
                i += 2;
            } else {
                flags.insert(name.to_string(), None);
                i += 1;
            }
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    Args { positional, flags }
}

impl Args {
    fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }
    fn value(&self, name: &str) -> Option<String> {
        self.flags.get(name).cloned().flatten()
    }
    fn number(&self, name: &str) -> Option<u64> {
        self.value(name).and_then(|v| v.parse().ok())
    }
}

fn rows() -> Vec<SessionRow> {
    let mut engine = ActivityEngine::default();
    let overlay = unpeel_serve::overlay::load();
    let keep = std::collections::HashSet::new();
    let listed = |model: &unpeel_serve::sessions::SidebarModel| -> Vec<SessionRow> {
        model
            .items
            .iter()
            .filter_map(|item| match item {
                SidebarItem::Session(i) => Some(model.rows[*i].clone()),
                _ => None,
            })
            .collect()
    };
    // Sidebar order, so `ls` and the UI agree. Worktree sessions are inline
    // items under their folder rows now, so one scan covers everything.
    let mut cache = ScanCache::default();
    listed(&scan_sidebar(
        &mut engine,
        overlay.as_ref(),
        &keep,
        &mut cache,
    ))
}

/// Resolve a session by id, id prefix, or exact title — scripts shouldn't
/// need full uuids.
fn resolve(reference: &str) -> Result<SessionRow, String> {
    let all = rows();
    if let Some(row) = all.iter().find(|r| r.id == reference) {
        return Ok(row.clone());
    }
    let matches: Vec<&SessionRow> = all
        .iter()
        .filter(|r| r.id.starts_with(reference) || r.label == reference)
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => Err(format!("no session matching {reference:?}")),
        n => Err(format!("{n} sessions match {reference:?} — use a full id")),
    }
}

fn session_json(row: &SessionRow) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "title": row.label,
        "command": row.command,
        "status": row.status.word(),
        "running": row.running,
        "project_id": row.project_id,
        "cwd": row.cwd,
        "pinned": row.pinned,
        "archived": row.archived,
        "created_at": row.created_at,
    })
}

/// Unescape the shell-ish sequences scripts type: \r \n \t \e \\ and \xNN.
fn unescape(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) => out.push(byte as char),
                    Err(_) => {
                        out.push_str("\\x");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn print_sessions(args: &Args) {
    let all = rows();
    if args.has("json") {
        let list: Vec<serde_json::Value> = all.iter().map(session_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&list).unwrap_or_default()
        );
        return;
    }
    for row in all {
        println!(
            "{}  {:9}  {:30}  {}",
            &row.id[..8.min(row.id.len())],
            row.status.word(),
            row.label.chars().take(30).collect::<String>(),
            row.command
        );
    }
}

const NEW_USAGE: &str = "usage: unpeel new [--command C | --preset LABEL] [--cwd DIR] [--project ID] [--cols N] [--rows N] [--json]

Create a new Session in this workspace. With no --command or --preset, opens a
plain terminal. Prints the new session id (or {\"id\":...} with --json).";

fn new_session(args: &Args) -> Result<(), String> {
    // `--help`/`-h`/`help` must print usage and create nothing. Without this
    // guard `unpeel new --help` fell through to "plain terminal" and spawned a
    // stray Session (the 0.4-era hazard; regressed on old CLIs probed by the
    // upgrade harness).
    if args.has("help")
        || args
            .positional
            .iter()
            .skip(1)
            .any(|arg| arg == "-h" || arg == "help")
    {
        println!("{NEW_USAGE}");
        return Ok(());
    }
    let command = match (args.value("preset"), args.value("command")) {
        (Some(label), _) => {
            let overlay = unpeel_serve::overlay::load();
            unpeel_serve::sessions::fallback_presets(overlay.as_ref())
                .into_iter()
                .find(|(l, _)| *l == label)
                .map(|(_, c)| c)
                .ok_or_else(|| format!("no preset labelled {label:?}"))?
        }
        (None, Some(command)) => command,
        (None, None) => String::new(), // plain terminal
    };
    let cwd = args
        .value("cwd")
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| "/".into());
    let project_id = args.value("project").unwrap_or_default();
    let label = unpeel_core::state::initial_session_label(&command, &cwd);
    let session = unpeel_core::state::SessionInfo {
        id: String::new(),
        project_id,
        label,
        custom_title: false,
        command,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        // The session Host binds this local CLI launch to the Host owner
        // before publishing its manifest.
        owner_principal_id: None,
        created_by_device_id: None,
        source_preset_id: None,
        tag_id: None,
        worktree_path: None,
        worktree_branch: None,
        parent_session_id: None,
        spawned_by: Some("cli".into()),
        role: None,
        task: None,
    };
    let cols = args.number("cols").unwrap_or(120) as u16;
    let rows_n = args.number("rows").unwrap_or(32) as u16;
    let id = unpeel_core::session_ops::spawn_session(session, &cwd, None, cols, rows_n)?;
    unpeel_core::session_host::wait_until_ready(&id, SESSION_READY_TIMEOUT)
        .map_err(|error| format!("session {id} did not become ready: {error}"))?;
    if args.has("json") {
        println!("{}", serde_json::json!({ "id": id }));
    } else {
        println!("{id}");
    }
    Ok(())
}

fn wait(args: &Args) -> Result<bool, String> {
    let reference = args
        .positional
        .get(1)
        .ok_or("usage: unpeel wait <id> [--idle] [--text S]")?;
    let row = resolve(reference)?;
    let timeout = Duration::from_secs(args.number("timeout").unwrap_or(300));
    let needle = args.value("text");
    let deadline = Instant::now() + timeout;
    // A fresh engine per poll would lose the hook latch, so keep one.
    let mut engine = ActivityEngine::default();
    let mut cache = ScanCache::default();
    while Instant::now() < deadline {
        if let Some(needle) = &needle {
            if let Ok(snapshot) = unpeel_serve::control::viewport_snapshot(&row.dir(), 0) {
                if snapshot
                    .viewport_rows
                    .iter()
                    .any(|r| r.text.contains(needle.as_str()))
                {
                    return Ok(true);
                }
            }
        } else {
            let overlay = unpeel_serve::overlay::load();
            let model = scan_sidebar(
                &mut engine,
                overlay.as_ref(),
                &std::collections::HashSet::new(),
                &mut cache,
            );
            if let Some(current) = model.rows.iter().find(|r| r.id == row.id) {
                match current.status {
                    // "settled" — idle covers done; exited ends the wait too.
                    Status::Idle | Status::Exited => return Ok(true),
                    Status::Attention if args.has("idle") => return Ok(true),
                    _ => {}
                }
            } else {
                return Ok(true); // session vanished: nothing left to wait for
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(false)
}

fn logs(args: &Args) -> Result<(), String> {
    let reference = args.positional.get(1).ok_or("usage: unpeel logs <id>")?;
    let row = resolve(reference)?;
    let path = row.dir().join("output.bin");
    let lines = args.number("lines").unwrap_or(200) as usize;
    // Approximate: read a generous tail, then keep the last N lines.
    let want = (lines * 400) as u64;
    let initial = unpeel_core::session_host::read_output_chunk(
        &row.id,
        None,
        Some(want.min(usize::MAX as u64) as usize),
        Some(want.min(usize::MAX as u64) as usize),
    )?;
    let text = String::from_utf8_lossy(&initial.data);
    let tail: Vec<&str> = text.lines().rev().take(lines).collect();
    let mut stdout = std::io::stdout();
    for line in tail.into_iter().rev() {
        let _ = writeln!(stdout, "{line}");
    }
    if !args.has("follow") {
        return Ok(());
    }
    let mut offset = initial.next_offset;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let current = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(offset);
        if current <= offset {
            if unpeel_core::session_ops::archived_marker(&row.id).is_some() {
                return Ok(());
            }
            continue;
        }
        let chunk = unpeel_core::session_host::read_output_chunk(
            &row.id,
            Some(offset),
            Some((current - offset).min(8 * 1024 * 1024) as usize),
            Some(want.min(usize::MAX as u64) as usize),
        )?;
        let start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
        if start != offset {
            let _ = stdout.write_all(b"\r\n[older output evicted]\r\n");
        }
        let _ = stdout.write_all(&chunk.data);
        let _ = stdout.flush();
        offset = chunk.next_offset;
    }
}

fn projects(args: &[String]) -> Result<(), String> {
    let state_path = app_paths::app_state_path();
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let state: serde_json::Value = std::fs::read(&state_path)
                .ok()
                .and_then(|raw| serde_json::from_slice(&raw).ok())
                .unwrap_or_default();
            if let Some(list) = state.get("projects").and_then(|v| v.as_array()) {
                for project in list {
                    println!(
                        "{:24} {}",
                        project.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        project.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                    );
                }
            }
            if let Some(overlay) = unpeel_serve::overlay::load() {
                for (_, name) in &overlay.projects {
                    println!("{name:24} (app-managed)");
                }
            }
            Ok(())
        }
        Some("add") => {
            let (Some(name), Some(path)) = (args.get(1), args.get(2)) else {
                return Err("usage: unpeel projects add <name> <path>".into());
            };
            match crate::state_cli::add_project_to_app_state(name, path)? {
                crate::state_cli::AddProject::Added => {}
                crate::state_cli::AddProject::Existing { name, .. } => {
                    return Err(format!("{name} already covers that folder"));
                }
            }
            println!("added {name} → {path}");
            Ok(())
        }
        Some("remove") => {
            let Some(needle) = args.get(1) else {
                return Err("usage: unpeel projects remove <name|path>".into());
            };
            remove_project(needle)
        }
        Some(other) => Err(format!("unknown projects subcommand: {other}")),
    }
}

/// `unpeel add [path]` — the one-liner: make the folder you're standing in
/// a project, so its sessions group in the sidebar (and on the phone). The
/// name comes from the directory, or the repo root when you're deeper
/// inside a checkout.
fn add_here(args: &Args) -> Result<(), String> {
    let raw = args
        .positional
        .get(1)
        .cloned()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .ok_or("could not resolve a path")?;
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        format!("{}/{rest}", std::env::var("HOME").unwrap_or_default())
    } else {
        raw
    };
    let path = std::fs::canonicalize(&expanded)
        .map_err(|e| format!("{expanded}: {e}"))?
        .to_string_lossy()
        .to_string();
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("{path} is not a directory"));
    }
    // Standing inside a repo? Offer its root — that's the project, not the
    // subdirectory you happen to be in.
    let root = unpeel_core::worktrees::repo_toplevel(&path).unwrap_or_else(|_| path.clone());
    let chosen = if root != path && !args.has("here") {
        println!("using the repo root {root} (--here to add this folder instead)");
        root
    } else {
        path
    };
    let name = args.value("name").unwrap_or_else(|| {
        std::path::Path::new(&chosen)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| chosen.clone())
    });
    match crate::state_cli::add_project_to_app_state(&name, &chosen)? {
        crate::state_cli::AddProject::Added => {}
        crate::state_cli::AddProject::Existing { name, .. } => {
            return Err(format!("{name} already covers that folder"));
        }
    }
    if args.has("json") {
        println!("{}", serde_json::json!({ "name": name, "path": chosen }));
    } else {
        println!("added {name} → {chosen}");
    }
    Ok(())
}

fn remove_project(needle: &str) -> Result<(), String> {
    unpeel_core::app_state::edit(|state| {
        let projects = state
            .get_mut("projects")
            .and_then(|v| v.as_array_mut())
            .ok_or("app-state.json has no projects array")?;
        let before = projects.len();
        projects.retain(|p| {
            p.get("name").and_then(|v| v.as_str()) != Some(needle)
                && p.get("path").and_then(|v| v.as_str()) != Some(needle)
        });
        if projects.len() == before {
            return Err(format!("no project matching {needle:?}"));
        }
        Ok(())
    })?;
    println!("removed {needle}");
    Ok(())
}

fn transcript(args: &Args) -> Result<(), String> {
    let reference = args
        .positional
        .get(1)
        .ok_or("usage: unpeel transcript <id>")?;
    let row = resolve(reference)?;
    let mode = if args.has("markdown") {
        "markdown"
    } else {
        "snapshot"
    };
    let host = unpeel_core::session_ops::resolve_host_binary()?;
    let mut command = std::process::Command::new(host);
    command.arg("__transcript__").arg(mode).arg(&row.id);
    if let Some(entries) = args.number("entries") {
        command.arg("--entries").arg(entries.to_string());
    }
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("transcript read failed".into())
    }
}

fn pair_through_running_host(
    advertised_host: Option<&str>,
    advertised_port: Option<u16>,
) -> Result<(), String> {
    let home = unpeel_core::app_paths::unpeel_home();
    let code = unpeel_serve::local_gateway::begin_pairing(&home, advertised_host, advertised_port)?;
    for line in unpeel_serve::pairing::qr_lines(&code) {
        println!("{line}");
    }
    println!("\n{code}\n");
    println!("paste or scan in an Unpeel Controller — expires in 5 minutes");
    loop {
        match unpeel_serve::local_gateway::pairing_status(&home)? {
            unpeel_serve::local_gateway::PairingStatus::Active => {
                std::thread::sleep(Duration::from_millis(100));
            }
            unpeel_serve::local_gateway::PairingStatus::Completed => {
                println!("paired");
                return Ok(());
            }
            unpeel_serve::local_gateway::PairingStatus::Closed => {
                return Err("pairing window closed without a device".into())
            }
        }
    }
}

/// Pairing always rides the Host service so the paired device lands in the
/// worker's live device list, not in a one-shot process that exits.
fn ensure_host_running() -> Result<(), String> {
    if unpeel_serve::driver::is_running() {
        return Ok(());
    }
    println!("{PAIR_STANDALONE_NOTICE}");
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    unpeel_serve::service::ensure_background(&executable)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if unpeel_serve::driver::is_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("the Unpeel Host did not start in time; run `unpeel serve` and retry".into())
}

fn paired_device_list() -> Result<Vec<serde_json::Value>, String> {
    let home = unpeel_core::app_paths::unpeel_home();
    unpeel_serve::local_gateway::paired_devices(&home)
}

fn device_field<'a>(device: &'a serde_json::Value, key: &str) -> &'a str {
    device
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

/// Resolve a paired device by exact id, then by unique name.
fn resolve_paired_device(selector: &str) -> Result<String, String> {
    let devices = paired_device_list()?;
    if let Some(device) = devices
        .iter()
        .find(|device| device_field(device, "id") == selector)
    {
        return Ok(device_field(device, "id").to_owned());
    }
    let named: Vec<&serde_json::Value> = devices
        .iter()
        .filter(|device| device_field(device, "name") == selector)
        .collect();
    match named.as_slice() {
        [device] => Ok(device_field(device, "id").to_owned()),
        [] => Err(format!(
            "no paired device {selector:?} (see `unpeel pair list`)"
        )),
        _ => Err(format!(
            "several paired devices are named {selector:?}; use the device id"
        )),
    }
}

fn pair_list(json: bool) -> Result<(), String> {
    ensure_host_running()?;
    let devices = paired_device_list()?;
    if json {
        println!("{}", serde_json::Value::Array(devices));
        return Ok(());
    }
    if devices.is_empty() {
        println!("no paired devices -- pair one: unpeel pair");
        return Ok(());
    }
    for device in &devices {
        let relay = device
            .get("relayAllowed")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        println!(
            "{:<38} {:<24} {:<10} {}",
            device_field(device, "id"),
            device_field(device, "name"),
            device_field(device, "platform"),
            if relay { "link" } else { "direct only" }
        );
    }
    Ok(())
}

fn pair_remove(selector: &str) -> Result<(), String> {
    ensure_host_running()?;
    let id = resolve_paired_device(selector)?;
    let home = unpeel_core::app_paths::unpeel_home();
    unpeel_serve::local_gateway::revoke_device(&home, &id)?;
    println!("unpaired: {id}");
    Ok(())
}

fn pair_relay(selector: &str, allowed: bool) -> Result<(), String> {
    ensure_host_running()?;
    let id = resolve_paired_device(selector)?;
    let home = unpeel_core::app_paths::unpeel_home();
    unpeel_serve::local_gateway::set_device_relay_allowed(&home, &id, allowed)?;
    println!(
        "relay {} for {id}",
        if allowed { "allowed" } else { "disabled" }
    );
    Ok(())
}

fn advertised_pairing_port(parsed: &Args) -> Result<Option<u16>, String> {
    parsed
        .value("advertise-port")
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| "--advertise-port must be between 1 and 65535".to_string())
                .and_then(|port| {
                    if port == 0 {
                        Err("--advertise-port must be between 1 and 65535".into())
                    } else {
                        Ok(port)
                    }
                })
        })
        .transpose()
}

fn serve() -> Result<(), String> {
    unpeel_serve::service::run(|event| {
        println!("{event}");
        let _ = std::io::stdout().flush();
    })
}

const HOSTS_USAGE: &str = "usage: unpeel hosts prune [--json]

Terminate leftover per-process session hosts in this workspace whose session
is already filed (exited or archived) but whose host process never exited.
Runs the same reap the Host service performs at startup and on a slow timer.
Only a provably-identical recorded host process (pid + start time) is signaled,
never the shared PTY core, never by name match. Prints what it reaped.";

/// `unpeel hosts prune` — user-only on-demand reap of orphaned session hosts.
fn hosts_prune(json: bool) -> Result<(), String> {
    let reaped = unpeel_core::session_host::reap_orphan_session_hosts();
    if json {
        let rows: Vec<serde_json::Value> = reaped
            .iter()
            .map(|host| {
                serde_json::json!({
                    "session_id": host.session_id,
                    "host_pid": host.host_pid,
                    "reason": host.reason.as_str(),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if reaped.is_empty() {
        println!("no leftover session hosts to prune");
    } else {
        for host in &reaped {
            println!(
                "reaped session host pid {} ({} session {})",
                host.host_pid,
                host.reason.as_str(),
                host.session_id
            );
        }
        let count = reaped.len();
        println!(
            "pruned {count} leftover session host{}",
            if count == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

const SERVE_USAGE: &str = "usage: unpeel serve [install [--graphical] | uninstall | status]

Run the UI-free Host service for the default and registered workspaces until
SIGINT or SIGTERM. With `--workspace NAME`, serve only that workspace.

  install      write the per-user service unit (launchd LaunchAgent on macOS,
               systemd --user unit on Linux), enable it, and start it
    --graphical  (Linux) bind the unit to graphical-session.target so the
               Host runs inside the desktop session — required for Computer
               Use (the engine needs the display and the session's
               accessibility bus); it starts when the session does
  uninstall    stop the managed service and remove the unit file only —
               workspace data and running Session terminals are untouched
  status       unit + live Host service state (exit 0 while it is running);
               on Linux also whether the unit is the desktop-session variant,
               whether graphical-session.target is active, and the desktop
               session visible to this shell

With `--workspace NAME`, install/uninstall/status manage a scoped
single-workspace unit instead of the machine service. Templates for manual
or image use: packaging/service/ (macOS needs auto-login on a headless Mac;
Linux needs `loginctl enable-linger`).";

/// Machine scope with no `UNPEEL_HOME`; a registered workspace otherwise —
/// the same rule `unpeel serve` itself uses to pick what it runs.
fn serve_unit_scope() -> Result<unpeel_serve::service_install::ServiceScope, String> {
    Ok(match crate::workspaces::current_scope()? {
        None => unpeel_serve::service_install::ServiceScope::Machine,
        Some((slug, home)) => unpeel_serve::service_install::ServiceScope::Workspace { slug, home },
    })
}

fn serve_service(action: &str, graphical: bool) -> Result<i32, String> {
    use unpeel_serve::service_install as install;

    let manager = install::ServiceManager::detect()?;
    let scope = serve_unit_scope()?;
    match action {
        "install" => {
            let binary = std::env::current_exe()
                .map_err(|error| format!("could not resolve the unpeel binary: {error}"))?;
            let path = install::install(manager, &scope, &binary, graphical)?;
            println!("installed {}", path.display());
            if graphical {
                println!("the Unpeel Host service now runs inside this user's desktop session (graphical-session.target)");
                println!("desktop session note: the session must import DISPLAY (`systemctl --user import-environment DISPLAY XAUTHORITY`) and pull in graphical-session.target — GNOME/KDE/sway do; an Xvfb or streamed-Xorg script starts packaging/service/unpeel-desktop-session.target instead (graphical-session.target refuses manual start)");
            } else {
                println!("the Unpeel Host service now starts on boot for this user");
            }
            if matches!(manager, install::ServiceManager::Launchd) {
                println!("headless Mac note: enable automatic login so the service starts after a reboot");
            } else {
                println!("headless box note: run `sudo loginctl enable-linger $USER` so it survives logout/boot");
            }
            Ok(0)
        }
        "uninstall" => {
            let path = install::uninstall(manager, &scope)?;
            println!("removed {}", path.display());
            println!("workspace data and running Session terminals were left untouched");
            Ok(0)
        }
        "status" => {
            let report = install::status(manager, &scope)?;
            println!(
                "unit: {} ({})",
                report.unit_path.display(),
                if report.unit_installed {
                    "installed"
                } else {
                    "not installed"
                }
            );
            println!("service manager: {}", report.manager_state);
            println!(
                "host service: {}",
                if report.serve_running {
                    "running"
                } else {
                    "not running"
                }
            );
            if report.unit_installed {
                println!(
                    "unit variant: {}",
                    if report.graphical_unit {
                        "desktop session (graphical-session.target)"
                    } else {
                        "plain (default.target)"
                    }
                );
            }
            if let Some(state) = &report.graphical_target_state {
                println!("graphical-session.target: {state}");
            }
            match &report.desktop_session {
                Ok(display) => println!("desktop session: {display}"),
                Err(reason) => println!("desktop session: none — {reason}"),
            }
            Ok(if report.serve_running { 0 } else { 1 })
        }
        _ => unreachable!("serve_service called with {action}"),
    }
}

/// Returns None when the arguments aren't a headless command (→ run the UI).
pub fn run(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        // Bare `unpeel` is never a blank screen: usage plus the one line
        // that says where the product actually runs.
        println!("{USAGE}");
        println!("{BARE_HINT}");
        return 0;
    };
    let parsed = parse(args);
    let reference_arg = || {
        parsed
            .positional
            .get(1)
            .cloned()
            .ok_or_else(|| format!("usage: unpeel {command} <id>"))
    };
    let result: Result<i32, String> = match command {
        "ls" | "list" | "--list" => {
            print_sessions(&parsed);
            Ok(0)
        }
        "new" => new_session(&parsed).map(|_| 0),
        "add" => add_here(&parsed).map(|_| 0),
        "send" => reference_arg().and_then(|reference| {
            let row = resolve(&reference)?;
            let mut text = parsed.positional[2..].join(" ");
            if parsed.has("enter") {
                text.push('\r');
            }
            unpeel_serve::control::send_text(&row.dir(), &text).map(|_| 0)
        }),
        "keys" => reference_arg().and_then(|reference| {
            let row = resolve(&reference)?;
            let sequence = unescape(&parsed.positional[2..].join(" "));
            unpeel_serve::control::send_text(&row.dir(), &sequence).map(|_| 0)
        }),
        "screen" | "--snapshot" => reference_arg().and_then(|reference| {
            let row = resolve(&reference)?;
            let cols = parsed.number("cols").unwrap_or(100) as u16;
            let rows_n = parsed.number("rows").unwrap_or(30) as u16;
            let snapshot =
                unpeel_serve::control::viewport_snapshot(&row.dir(), 0).or_else(|_| {
                    unpeel_core::terminal_viewport::read_terminal_viewport_snapshot(
                        row.id.clone(),
                        cols,
                        rows_n,
                        None,
                        None,
                        None,
                    )
                })?;
            for line in &snapshot.viewport_rows {
                println!("{}", line.text.trim_end());
            }
            Ok(0)
        }),
        "logs" | "tail" => logs(&parsed).map(|_| 0),
        "wait" => wait(&parsed).map(|settled| if settled { 0 } else { 1 }),
        "restart" | "resume" => reference_arg().and_then(|reference| {
            let row = resolve(&reference)?;
            if row.running {
                if !row.resume_agent_available {
                    return Err(crate::state_cli::resume_unavailable_message(&row).into());
                }
                unpeel_core::session_ops::resume_agent(&row.id)?;
                // In-place resume deliberately preserves the Session/PTY id.
                println!("{}", row.id);
            } else {
                if !row.resume_available {
                    return Err("this session cannot be resumed".into());
                }
                let id = unpeel_core::session_ops::resume_session(&row.id, None, 120, 32)?;
                unpeel_core::session_host::wait_until_ready(&id, SESSION_READY_TIMEOUT)
                    .map_err(|error| format!("session {id} did not become ready: {error}"))?;
                println!("{id}");
            }
            Ok(0)
        }),
        "stop" => reference_arg()
            .and_then(|reference| resolve(&reference))
            .and_then(|row| unpeel_core::session_ops::stop_session(&row.id).map(|_| 0)),
        "archive" => reference_arg()
            .and_then(|reference| resolve(&reference))
            .and_then(|row| unpeel_core::session_ops::archive_session(&row.id).map(|_| 0)),
        "restore" => reference_arg()
            .and_then(|reference| resolve(&reference))
            .and_then(|row| unpeel_core::session_ops::restore_session(&row.id).map(|_| 0)),
        "rm" | "remove" | "close" => reference_arg()
            .and_then(|reference| resolve(&reference))
            .and_then(|row| unpeel_core::session_ops::remove_session(&row.id).map(|_| 0)),
        "transcript" => transcript(&parsed).map(|_| 0),
        "open" => Ok(crate::open_cli::run(&args[1..])),
        "settings" => match args.get(1).map(String::as_str) {
            Some("--help" | "-h" | "help") if args.len() == 2 => {
                println!("{}", crate::settings_cli::HELP);
                Ok(0)
            }
            _ => crate::settings_cli::run(&parsed.positional[1..], parsed.has("json")).map(|_| 0),
        },
        "apps" => Ok(crate::apps_cli::run(&args[1..])),
        // Lane 5 (2026-09-03): the one Browser MCP engine verb; the logic
        // lives in unpeel_core::browser_engine, this is only the dispatch.
        "browser" => Ok(crate::browser_cli::run(&args[1..])),
        // Lane A (2026-09-03): the one Computer Use engine verb, same shape.
        "computer" => Ok(crate::computer_cli::run(&args[1..])),
        "link" => Ok(crate::link_cli::run(
            &parsed.positional[1..],
            parsed.has("json"),
        )),
        "presets" => crate::state_cli::presets_cli(&args[1..]).map(|_| 0),
        "workspaces" => {
            crate::workspaces::cli(&parsed.positional[1..], parsed.has("json")).map(|_| 0)
        }
        "profiles" | "profile" => Err("`profiles` was renamed; use `unpeel workspaces`".into()),
        "hosts" => match args.get(1).map(String::as_str) {
            Some("prune")
                if parsed.positional.len() == 2
                    && parsed.flags.keys().all(|flag| flag == "json") =>
            {
                hosts_prune(parsed.has("json")).map(|_| 0)
            }
            Some("--help" | "-h" | "help") if parsed.positional.len() == 2 => {
                println!("{HOSTS_USAGE}");
                Ok(0)
            }
            _ => Err(HOSTS_USAGE.into()),
        },
        "projects" => projects(&args[1..]).map(|_| 0),
        "serve" => match args.get(1).map(String::as_str) {
            None => serve().map(|_| 0),
            Some(action @ ("install" | "uninstall" | "status"))
                if parsed.positional.len() == 2
                    && parsed
                        .flags
                        .keys()
                        .all(|flag| flag == "graphical" || flag == "workspace") =>
            {
                if parsed.has("graphical") && action != "install" {
                    Err(SERVE_USAGE.into())
                } else {
                    serve_service(action, parsed.has("graphical"))
                }
            }
            Some("--help" | "-h" | "help") if args.len() == 2 => {
                println!("{SERVE_USAGE}");
                Ok(0)
            }
            _ => Err(SERVE_USAGE.into()),
        },
        "pair" => match args.get(1).map(String::as_str) {
            Some("--help" | "-h" | "help") => {
                println!("{PAIR_USAGE}");
                Ok(0)
            }
            Some("list" | "ls") => pair_list(parsed.has("json")).map(|_| 0),
            Some("remove" | "rm" | "unpair" | "revoke") => match parsed.positional.get(2) {
                Some(device) if parsed.positional.len() == 3 => pair_remove(device).map(|_| 0),
                _ => Err(PAIR_USAGE.into()),
            },
            Some("relay") => match (parsed.positional.get(2), parsed.positional.get(3)) {
                (Some(device), Some(state)) if parsed.positional.len() == 4 => {
                    let allowed = match state.as_str() {
                        "on" | "true" | "allow" => true,
                        "off" | "false" | "deny" => false,
                        _ => {
                            eprintln!("{PAIR_USAGE}");
                            return 2;
                        }
                    };
                    pair_relay(device, allowed).map(|_| 0)
                }
                _ => Err(PAIR_USAGE.into()),
            },
            Some(other) if !other.starts_with("--") => Err(PAIR_USAGE.into()),
            _ => (|| {
                let advertised_port = advertised_pairing_port(&parsed)?;
                ensure_host_running()?;
                pair_through_running_host(
                    parsed.value("advertise-host").as_deref(),
                    advertised_port,
                )
            })()
            .map(|_| 0),
        },
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(0)
        }
        "version" | "--version" | "-V" => {
            println!("unpeel {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        other => {
            eprintln!("unpeel: unknown command {other:?}\n");
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_parsing() {
        let args: Vec<String> = ["send", "abc", "hello", "world", "--enter"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse(&args);
        assert_eq!(parsed.positional, ["send", "abc", "hello", "world"]);
        assert!(parsed.has("enter"));
        let args: Vec<String> = ["wait", "abc", "--timeout", "12", "--idle"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse(&args);
        assert_eq!(parsed.number("timeout"), Some(12));
        assert!(parsed.has("idle"));

        let args: Vec<String> = ["pair", "--serve"].iter().map(|s| s.to_string()).collect();
        let parsed = parse(&args);
        assert_eq!(parsed.positional, ["pair"]);
        assert!(parsed.has("serve"));
    }

    #[test]
    fn escape_sequences() {
        assert_eq!(unescape("hi\\r"), "hi\r");
        assert_eq!(unescape("\\e[A"), "\x1b[A");
        assert_eq!(unescape("\\x03"), "\u{3}");
        assert_eq!(unescape("plain"), "plain");
        assert_eq!(unescape("back\\\\slash"), "back\\slash");
    }

    #[test]
    fn bare_and_unknown_commands_never_open_a_ui() {
        assert_eq!(run(&[]), 0);
        assert_eq!(run(&["definitely-not-a-verb".to_string()]), 2);
    }

    #[test]
    fn stale_profile_commands_are_rejected() {
        for command in ["profiles", "profile"] {
            let args = vec![command.to_string()];
            assert_eq!(run(&args), 1);
        }
    }
}
