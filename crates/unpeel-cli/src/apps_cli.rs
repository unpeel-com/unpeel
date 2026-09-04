//! `unpeel apps` — inspect and install official Host-side Unpeel Apps.

use unpeel_core::{app_installer, apps_mcp};

pub const HELP: &str = "\
unpeel apps — Host-side Unpeel Apps

  unpeel apps list [--json]
  unpeel apps install <app-id> [--check] [--json]

Apps install under ~/.unpeel/apps/bin after the release tarball is verified
against its mandatory SHA-256 sidecar. --check never downloads anything.";

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let check = args.iter().any(|arg| arg == "--check");
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
        ["install", app_id] => install(app_id, check, json),
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
            println!("{:<28} {:<8} {}", status.id, status.state, status.command);
        }
    }
    0
}

fn install(app_id: &str, check: bool, json: bool) -> i32 {
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
