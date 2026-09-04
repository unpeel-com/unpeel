//! `unpeel computer` — the Host-owned Computer Use engine, scripted.
//!
//! The only computer verb the CLI carries: it delegates every decision to
//! `unpeel_core::computer_engine` (the pinned manifest, archive + member
//! hash verification, the locked install, the shared resolution order), so
//! the headless Host, the workspace worker's on-demand install, and the MCP
//! server can never disagree about which engine is "the" engine.
//!
//! Exit codes: 0 engine ready · 1 failure (download/hash/unsupported, or the
//! installed binary cannot start — e.g. missing X11 client libraries, named
//! with the apt line) · 3 `--check` found the engine absent or stale (nothing
//! was installed) · 4 engine present and runnable but no desktop session is
//! visible to this process (Linux; the daemon needs the desktop's
//! DISPLAY/WAYLAND_DISPLAY and its session bus).

use unpeel_core::computer_engine as engine;

pub const HELP: &str = "\
unpeel computer — Host-owned Computer Use engine (cua-driver)

  unpeel computer install [--check] [--json]
      install (or confirm) the pinned engine under ~/.unpeel/computer/bin
      after sha256 verification against protocol/computer-engine-v1.json
      (the release archive, then the one extracted binary).
      --check only reports: exit 0 ready, 1 installed but cannot start
      (missing shared libraries are named with the apt line), 3 missing/stale,
      4 no desktop session.

The daemon runs inside a desktop session: on Linux start `unpeel serve`
from the X11/Wayland session (or `unpeel serve install --graphical`); on
macOS the Unpeel app owns it. Override the engine with
UNPEEL_CUA_DRIVER_BIN=<path>; UNPEEL_COMPUTER_ENGINE_INSTALL=0 keeps the
Host service from installing it on demand.";

/// `args` are the raw words after `computer` (flags parsed here so this verb
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
            eprintln!("unknown computer action: {other}\n{HELP}");
            1
        }
    }
}

fn install(check_only: bool, json: bool) -> i32 {
    let home = unpeel_core::app_paths::unpeel_home();
    let resolved = if check_only {
        engine::resolve(&home).map_err(|error| (engine::Status::missing(Some(error)), 3))
    } else {
        engine::ensure_installed(&home).map_err(|error| (engine::Status::failed(error), 1))
    };
    let (status, code) = match resolved {
        Err(failed) => failed,
        // Every hash can verify and the binary still not start (a bare
        // image without the X11 client libraries): run it once and report
        // that as `failed`, exit 1, naming the libraries and the apt line.
        Ok(path) => match engine::probe(&path) {
            Ok(version_line) => {
                let mut status = engine::Status::ready(path.clone());
                if path != engine::binary_path(&home) {
                    // An override, bundled, or PATH copy: usable, but only
                    // the managed copy is hash-verified against the pin, so
                    // report what it says it is, not the pin.
                    status.version = format!(
                        "{} (unmanaged copy, not hash-verified; pinned {})",
                        version_line
                            .strip_prefix("cua-driver ")
                            .unwrap_or(&version_line),
                        status.version
                    );
                }
                (status, 0)
            }
            Err(failure) => (engine::Status::failed(failure.to_string()), 1),
        },
    };
    let session = engine::graphical_session();
    let code = if code == 0 && session.is_none() {
        4
    } else {
        code
    };
    report(&status, session.as_deref(), json);
    code
}

fn report(status: &engine::Status, session: Option<&str>, json: bool) {
    if json {
        let mut value = serde_json::to_value(status).unwrap_or_default();
        value["session"] = match session {
            Some(label) => serde_json::json!({ "display": label }),
            None => {
                serde_json::json!({ "display": null, "error": engine::missing_session_message() })
            }
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return;
    }
    println!("engine:  cua-driver {} — {}", status.version, status.state);
    if let Some(path) = &status.path {
        println!("path:    {}", path.display());
    }
    if let Some(error) = &status.error {
        println!("error:   {error}");
    }
    match session {
        Some(label) => println!("session: {label}"),
        None => println!("session: {}", engine::missing_session_message()),
    }
}
