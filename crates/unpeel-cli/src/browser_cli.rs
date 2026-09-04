//! `unpeel browser` — the Host-owned Browser MCP engine, scripted.
//!
//! The only browser verb the CLI carries: it delegates every decision to
//! `unpeel_core::browser_engine` (the pinned manifest, hash verification,
//! the locked install, the shared resolution order), so the headless Host,
//! the workspace worker's start-time install, and the MCP server can never
//! disagree about which engine is "the" engine.
//!
//! Exit codes: 0 engine ready · 1 failure (download/hash/unsupported) ·
//! 3 `--check` found the engine absent or stale (nothing was installed) ·
//! 4 engine present but no system Chrome/Chromium on this Host.

use std::path::PathBuf;

use unpeel_core::browser_engine as engine;

pub const HELP: &str = "\
unpeel browser — Host-owned Browser MCP engine (agent-browser)

  unpeel browser install [--check] [--json]
      install (or confirm) the pinned engine under ~/.unpeel/browser/bin
      after sha256 verification against protocol/browser-engine-v1.json.
      --check only reports: exit 0 ready, 3 missing/stale, 4 no browser.

The engine drives a system Chrome/Chromium; Unpeel never installs one.
Override the engine with UNPEEL_AGENT_BROWSER_BIN=<path>.";

/// `args` are the raw words after `browser` (flags parsed here so this verb
/// owns its own `--check` / `--json` without touching the shared parser).
pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let check = args.iter().any(|a| a == "--check");
    match args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(String::as_str)
    {
        Some("install") => install(check, json),
        Some("--help" | "-h" | "help") | None => {
            println!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("unknown browser action: {other}\n{HELP}");
            1
        }
    }
}

fn install(check_only: bool, json: bool) -> i32 {
    let home = unpeel_core::app_paths::unpeel_home();
    let pinned = engine::pinned();
    let path_dirs = unpeel_core::setup::search_dirs();
    let (status, code) = if check_only {
        match engine::resolve(&home) {
            Ok(path) => (engine::Status::ready(path), 0),
            Err(error) => (
                engine::Status {
                    state: "missing".into(),
                    version: pinned.version.clone(),
                    path: None,
                    error: Some(error),
                },
                3,
            ),
        }
    } else {
        match engine::ensure_installed(&home) {
            Ok(path) => (engine::Status::ready(path), 0),
            Err(error) => (engine::Status::failed(error), 1),
        }
    };
    let browser = engine::system_browser(&path_dirs);
    let code = if code == 0 && browser.is_none() {
        4
    } else {
        code
    };
    report(&status, browser, &path_dirs, json);
    code
}

fn report(status: &engine::Status, browser: Option<PathBuf>, path_dirs: &[PathBuf], json: bool) {
    if json {
        let mut value = serde_json::to_value(status).unwrap_or_default();
        value["browser"] = match &browser {
            Some(path) => serde_json::json!({ "path": path }),
            None => {
                serde_json::json!({ "path": null, "error": engine::missing_browser_message(path_dirs) })
            }
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return;
    }
    println!(
        "engine:  agent-browser {} — {}",
        status.version, status.state
    );
    if let Some(path) = &status.path {
        println!("path:    {}", path.display());
    }
    if let Some(error) = &status.error {
        println!("error:   {error}");
    }
    match browser {
        Some(path) => println!("browser: {}", path.display()),
        None => println!("browser: {}", engine::missing_browser_message(path_dirs)),
    }
}
