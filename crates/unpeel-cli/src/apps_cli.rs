//! `unpeel apps` — inspect and install official Host-side Unpeel Apps.

use std::io::{self, IsTerminal, Write};

use unpeel_core::{app_installer, apps_mcp};

pub const HELP: &str = "\
unpeel apps — Host-side Unpeel Apps

  unpeel apps list [--json]
  unpeel apps install <app-id> [--check] [--yes] [--json]
  unpeel apps update [<app-id> | --all] [--check] [--yes] [--json]
  unpeel apps link <app-id> <executable>
  unpeel apps unlink <app-id>

Apps install under ~/.unpeel/apps/bin after the release tarball is verified
against its mandatory SHA-256 sidecar. --check never downloads anything.
Interactive installs ask first; noninteractive installs require --yes.

update reinstalls an App whose installed version differs from the one this
Host's registry publishes (--check only reports; exit 3 = update available).
Running instances keep their old binary until restarted.

link is development mode: it points the managed slot at a local build (a
symlink, so every rebuild is picked up by the next launch) instead of a
downloaded release. unlink removes only such a link.";

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let check = args.iter().any(|arg| arg == "--check");
    let yes = args.iter().any(|arg| arg == "--yes");
    if let Some(flag) = args.iter().find(|arg| {
        arg.starts_with("--") && !matches!(arg.as_str(), "--json" | "--check" | "--yes" | "--all")
    }) {
        eprintln!("unknown apps option {flag:?}\n\n{HELP}");
        return 1;
    }
    let positional: Vec<&str> = args
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .collect();
    match positional.as_slice() {
        [] | ["help"] => {
            println!("{HELP}");
            0
        }
        ["list"] => list(json),
        ["install", app_id] => install(app_id, check, yes, json),
        ["update", app_id] => update(Some(app_id), check, yes, json),
        // No id (with or without --all) means every installed App.
        ["update"] => update(None, check, yes, json),
        ["link", app_id, executable] => link(app_id, executable),
        ["unlink", app_id] => unlink(app_id),
        _ => {
            eprintln!("{HELP}");
            1
        }
    }
}

fn list(json: bool) -> i32 {
    let home = unpeel_core::app_paths::unpeel_home();
    let statuses: Vec<_> = apps_mcp::catalog_apps()
        .iter()
        .map(|app| app_installer::status(&home, app))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&statuses).unwrap_or_default()
        );
    } else {
        for status in statuses {
            let version = match (&status.installed_version, &status.version) {
                (Some(installed), Some(latest)) if status.update_available => {
                    format!("{installed} → {latest}")
                }
                (Some(installed), _) => installed.clone(),
                (None, Some(latest)) if status.update_available => format!("? → {latest}"),
                (None, Some(latest)) if status.state == "linked" => format!("dev ({latest})"),
                (None, Some(latest)) => latest.clone(),
                (None, None) => String::new(),
            };
            println!(
                "{:<28} {:<8} {:<22} {}",
                status.id, status.state, status.command, version
            );
        }
    }
    0
}

fn install(app_id: &str, check: bool, yes: bool, json: bool) -> i32 {
    let Some(app) = apps_mcp::catalog_app(app_id) else {
        eprintln!("unknown or unsupported App id {app_id:?}");
        return 1;
    };
    let home = unpeel_core::app_paths::unpeel_home();
    let mut status = app_installer::status(&home, &app);
    let code = if status.state == "ready" {
        0
    } else if check {
        3
    } else {
        if !yes {
            match confirm_install(&app.name) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!("Installation cancelled.");
                    return 1;
                }
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            }
        }
        match app_installer::install(&home, &app.id) {
            Ok(path) => {
                status.state = "ready".into();
                status.path = Some(path);
                0
            }
            Err(error) => {
                status.state = "failed".into();
                if json {
                    println!("{}", serde_json::json!({ "app": status, "error": error }));
                } else {
                    eprintln!("{error}");
                }
                return 1;
            }
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).unwrap_or_default()
        );
    } else {
        println!("{}: {}", status.name, status.state);
        if let Some(path) = status.path {
            println!("path: {}", path.display());
        }
    }
    code
}

fn confirm_install(app_name: &str) -> Result<bool, String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(format!(
            "Refusing to install {app_name} non-interactively. Ask the user to run this command in a terminal, or pass --yes from user-owned automation."
        ));
    }
    eprint!("Install {app_name} in this workspace? [y/N] ");
    io::stderr()
        .flush()
        .map_err(|error| format!("show install prompt: {error}"))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("read install answer: {error}"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn link(app_id: &str, executable: &str) -> i32 {
    let home = unpeel_core::app_paths::unpeel_home();
    let path = std::path::Path::new(executable);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    match app_installer::link(&home, app_id, &path) {
        Ok(target) => {
            unpeel_core::state_bus::flush();
            println!("{} -> {}", target.display(), path.display());
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn unlink(app_id: &str) -> i32 {
    let home = unpeel_core::app_paths::unpeel_home();
    match app_installer::unlink(&home, app_id) {
        Ok(removed) => {
            unpeel_core::state_bus::flush();
            println!(
                "{}",
                if removed {
                    "unlinked"
                } else {
                    "nothing linked"
                }
            );
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// Reinstall the Apps whose installed release differs from the registry's
/// version. One App, or every installed one with `--all`. `--check` only
/// reports: exit 3 when at least one update is available, 0 when none.
fn update(app_id: Option<&str>, check: bool, yes: bool, json: bool) -> i32 {
    let home = unpeel_core::app_paths::unpeel_home();
    let candidates: Vec<_> = match app_id {
        Some(app_id) => match apps_mcp::catalog_app(app_id) {
            Some(app) => vec![app],
            None => {
                eprintln!("unknown or unsupported App id {app_id:?}");
                return 1;
            }
        },
        None => apps_mcp::catalog_apps(),
    };
    let outdated: Vec<_> = candidates
        .into_iter()
        .filter(|app| app_installer::status(&home, app).update_available)
        .collect();
    if outdated.is_empty() {
        if json {
            println!("{}", serde_json::json!({ "updated": [], "available": [] }));
        } else {
            println!("Every installed App is current.");
        }
        return 0;
    }
    if check {
        if json {
            println!(
                "{}",
                serde_json::json!({ "available": outdated.iter().map(|app| &app.id).collect::<Vec<_>>() })
            );
        } else {
            for app in &outdated {
                let status = app_installer::status(&home, app);
                println!(
                    "{}: {} → {}",
                    app.id,
                    status.installed_version.as_deref().unwrap_or("?"),
                    app.version.as_deref().unwrap_or("?")
                );
            }
        }
        return 3;
    }
    if !yes {
        let names: Vec<_> = outdated.iter().map(|app| app.name.as_str()).collect();
        match confirm_install(&names.join(", ")) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Update cancelled.");
                return 1;
            }
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        }
    }
    let mut updated = Vec::new();
    let mut failed = Vec::new();
    for app in &outdated {
        match app_installer::install(&home, &app.id) {
            Ok(_) => updated.push(app.id.clone()),
            Err(error) => {
                eprintln!("{}: {error}", app.id);
                failed.push(app.id.clone());
            }
        }
    }
    unpeel_core::state_bus::flush();
    if json {
        println!(
            "{}",
            serde_json::json!({ "updated": updated, "failed": failed })
        );
    } else {
        for id in &updated {
            println!("updated {id} (running instances keep the old binary until restarted)");
        }
    }
    if failed.is_empty() {
        0
    } else {
        1
    }
}
