//! `unpeel open` — resolve a typed resource through this workspace's Host
//! catalog and opener policy, then use the shared user-owned App open path.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use unpeel_core::app_presentations::AppResourceRef;
use unpeel_core::{app_installer, app_open, apps_mcp};

pub const HELP: &str = "\
unpeel open — open a resource with a workspace App

  unpeel open <path> [--with <app-id>] [--media-type <type>] [--json]
  unpeel open git:working-tree [--with diffs] [--json]
  unpeel open <resource-id> --kind <resource-kind> [--with <app-id>] [--json]
  unpeel open <path> --resolve [--json]

Inside an Unpeel Session, the App opens in a companion pane. Outside one, it
opens as a new hosted Session. Missing Apps require user confirmation.
--resolve reports which opener this workspace's policy picks for the
resource (an App, the editor, or the system) without opening anything —
what an App uses before acting on a file the user activated inside it.";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    resource: String,
    app: Option<String>,
    kind: Option<String>,
    media_type: Option<String>,
    json: bool,
    resolve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedResource {
    kind: String,
    id: String,
    media_type: Option<String>,
}

impl ResolvedResource {
    fn selector(&self) -> String {
        apps_mcp::resource_selector(&self.kind, self.media_type.as_deref())
    }
}

pub fn run(arguments: &[String]) -> i32 {
    match run_inner(arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run_inner(arguments: &[String]) -> Result<i32, String> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "help" | "-h" | "--help"))
    {
        println!("{HELP}");
        return Ok(0);
    }
    let args = parse(arguments)?;
    let resource = resolve_resource(&args)?;
    // A file no App claims (unknown extension, no --media-type) has no
    // catalog selector to look up: it belongs to the editor unless the
    // caller named an App explicitly.
    if resource.kind == "file" && resource.media_type.is_none() && args.app.is_none() {
        if args.resolve {
            return report_plain_file(&args, &resource);
        }
        let state = unpeel_core::app_state::load().unwrap_or_else(|_| json!({}));
        return launch_editor(&state, &resource);
    }
    let selector = resource.selector();
    let state = unpeel_core::app_state::load().unwrap_or_else(|_| json!({}));
    let configured = unpeel_core::controller_host::wire_openers(&state)
        .get(&selector)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    if args.resolve {
        return report_resolution(&args, &resource, configured.as_deref());
    }

    if args.app.is_none() {
        match configured.as_deref() {
            Some("editor") => return launch_editor(&state, &resource),
            Some("system") => return launch_system(&resource),
            _ => {}
        }
    }

    let app = match args.app.as_deref() {
        Some(wanted) => apps_mcp::resolve_catalog_app(wanted)?,
        None => match configured
            .as_deref()
            .and_then(|opener| opener.strip_prefix("app:"))
        {
            Some(wanted) => apps_mcp::resolve_catalog_app(wanted)?,
            None => apps_mcp::default_catalog_app(&selector)?,
        },
    };
    if !apps_mcp::catalog_app_handles(&app, &resource.kind, resource.media_type.as_deref()) {
        return Err(format!("{} does not handle {selector}.", app.name));
    }

    let home = unpeel_core::app_paths::unpeel_home();
    let mut status = app_installer::status(&home, &app);
    if status.state != "ready" {
        if !confirm_install(&app.name)? {
            return Err(format!(
                "{} is not installed. Install it from Open resources settings or run `unpeel apps install {}`.",
                app.name, app.id
            ));
        }
        let path = app_installer::install(&home, &app.id)?;
        status.state = "ready".into();
        status.path = Some(path);
    }

    if let Some(caller_session_id) = caller_session_id() {
        let result = app_open::open_app(
            &app_open::OpenAppRequest::panel(
                caller_session_id,
                app.id.clone(),
                Some(AppResourceRef {
                    kind: resource.kind.clone(),
                    id: resource.id.clone(),
                }),
                resource.media_type.clone(),
            ),
            None,
        )?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "app": { "id": result.app_id, "name": result.app_name },
                    "resource": {
                        "kind": resource.kind,
                        "id": resource.id,
                        "mediaType": resource.media_type,
                    },
                    "presentation": result.presentation.agent_receipt(),
                    "processState": result.process_state,
                }))
                .map_err(|error| format!("encode open result: {error}"))?
            );
        } else {
            println!("Opened {} in an App pane.", result.app_name);
        }
        return Ok(0);
    }

    let cwd = absolute_path(Path::new("."))?
        .to_string_lossy()
        .into_owned();
    let result = app_open::open_standalone_app(
        &app_open::OpenStandaloneAppRequest {
            app_id: app.id.clone(),
            resource: Some(AppResourceRef {
                kind: resource.kind.clone(),
                id: resource.id.clone(),
            }),
            media_type: resource.media_type.clone(),
            cwd,
            project_id: String::new(),
        },
        None,
    )?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "app": { "id": result.app_id, "name": result.app_name },
                "resource": {
                    "kind": resource.kind,
                    "id": resource.id,
                    "mediaType": resource.media_type,
                },
                "sessionID": result.session_id,
            }))
            .map_err(|error| format!("encode open result: {error}"))?
        );
    } else {
        println!("{}", result.session_id);
    }
    Ok(0)
}

fn parse(arguments: &[String]) -> Result<Args, String> {
    let mut resource = None;
    let mut app = None;
    let mut kind = None;
    let mut media_type = None;
    let mut json = false;
    let mut resolve = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--json" => json = true,
            "--resolve" => resolve = true,
            "--with" | "--kind" | "--media-type" => {
                let flag = arguments[index].as_str();
                let value = arguments
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| format!("{flag} requires a value"))?
                    .clone();
                match flag {
                    "--with" => app = Some(value),
                    "--kind" => kind = Some(value),
                    _ => media_type = Some(value),
                }
                index += 1;
            }
            argument if argument.starts_with('-') => {
                return Err(format!("unknown open option {argument:?}\n\n{HELP}"));
            }
            argument => {
                if resource.replace(argument.to_string()).is_some() {
                    return Err(format!("open accepts one resource\n\n{HELP}"));
                }
            }
        }
        index += 1;
    }
    Ok(Args {
        resource: resource.ok_or_else(|| HELP.to_string())?,
        app,
        kind,
        media_type,
        json,
        resolve,
    })
}

/// `--resolve`: say which opener the workspace policy picks for the resource
/// — `app:<id>` (with its install state), `editor`, or `system` — without
/// opening anything. An explicit `--with` short-circuits to that App.
fn report_resolution(
    args: &Args,
    resource: &ResolvedResource,
    configured: Option<&str>,
) -> Result<i32, String> {
    let selector = resource.selector();
    let mut opener = match (args.app.as_deref(), configured) {
        (Some(wanted), _) => format!("app:{}", apps_mcp::resolve_catalog_app(wanted)?.id),
        (None, Some(opener @ ("editor" | "system"))) => opener.to_string(),
        (None, Some(opener)) if opener.starts_with("app:") => opener.to_string(),
        (None, _) => match apps_mcp::default_catalog_app(&selector) {
            Ok(app) => format!("app:{}", app.id),
            // No App claims it: a file still has the editor; anything else
            // has no opener at all.
            Err(_) if resource.kind == "file" => "editor".to_string(),
            Err(error) => return Err(error),
        },
    };
    let mut app_json = serde_json::Value::Null;
    if let Some(app_id) = opener.strip_prefix("app:") {
        let app = apps_mcp::resolve_catalog_app(app_id)?;
        if !apps_mcp::catalog_app_handles(&app, &resource.kind, resource.media_type.as_deref()) {
            return Err(format!("{} does not handle {selector}.", app.name));
        }
        let status = app_installer::status(&unpeel_core::app_paths::unpeel_home(), &app);
        opener = format!("app:{}", app.id);
        app_json = json!({
            "id": app.id,
            "name": app.name,
            "installed": status.state != "missing",
        });
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "opener": opener,
                "app": app_json,
                "resource": {
                    "kind": resource.kind,
                    "id": resource.id,
                    "mediaType": resource.media_type,
                },
                "selector": selector,
            }))
            .map_err(|error| format!("encode resolution: {error}"))?
        );
    } else {
        println!("{opener}");
    }
    Ok(0)
}

fn resolve_resource(args: &Args) -> Result<ResolvedResource, String> {
    if args.resource == "git:working-tree" && args.kind.is_none() {
        return Ok(ResolvedResource {
            kind: "git.working-tree".into(),
            id: absolute_path(Path::new("."))?
                .to_string_lossy()
                .into_owned(),
            media_type: None,
        });
    }
    if let Some(kind) = args.kind.as_deref() {
        if !apps_mcp::valid_resource_kind(kind) {
            return Err(format!("invalid resource kind {kind:?}"));
        }
        let id = if path_backed(kind) {
            absolute_path(Path::new(&args.resource))?
                .to_string_lossy()
                .into_owned()
        } else {
            args.resource.clone()
        };
        let media_type = if kind == "file" {
            resolve_file_media_type(&id, args.media_type.as_deref(), args.app.is_some())?
        } else {
            if args.media_type.is_some() {
                return Err("--media-type is valid only with a file resource".into());
            }
            None
        };
        return Ok(ResolvedResource {
            kind: kind.to_ascii_lowercase(),
            id,
            media_type,
        });
    }

    let path = absolute_path(Path::new(&args.resource))?;
    let kind = if path.is_dir() { "folder" } else { "file" };
    let id = path.to_string_lossy().into_owned();
    let media_type = (kind == "file")
        .then(|| resolve_file_media_type(&id, args.media_type.as_deref(), args.app.is_some()))
        .transpose()?
        .flatten();
    Ok(ResolvedResource {
        kind: kind.into(),
        id,
        media_type,
    })
}

/// The media type for a path: explicit `--media-type`, else the registry's
/// extension map. Unknown extensions are an error only when the caller named
/// an App (`--with`), which needs a selector to validate against; otherwise
/// `None` means "no App claims this file" and the editor handles it.
fn resolve_file_media_type(
    path: &str,
    explicit: Option<&str>,
    app_named: bool,
) -> Result<Option<String>, String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(Some(explicit.to_string()));
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match apps_mcp::media_type_for_extension(extension) {
        Some(media_type) => Ok(Some(media_type)),
        None if app_named => Err(format!(
            "No App declares the .{extension} extension. Pass --media-type or add it to the App registry."
        )),
        None => Ok(None),
    }
}

/// `--resolve` for a file no App claims: the editor, reported in the same
/// shape as `report_resolution`.
fn report_plain_file(args: &Args, resource: &ResolvedResource) -> Result<i32, String> {
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "opener": "editor",
                "app": serde_json::Value::Null,
                "resource": {
                    "kind": resource.kind,
                    "id": resource.id,
                    "mediaType": serde_json::Value::Null,
                },
                "selector": "file",
            }))
            .map_err(|error| format!("encode resolution: {error}"))?
        );
    } else {
        println!("editor");
    }
    Ok(0)
}

fn path_backed(kind: &str) -> bool {
    matches!(kind, "file" | "folder" | "git.working-tree")
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Ok(canonical);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("read current directory: {error}"))?
            .join(path)
    };
    let Some(parent) = absolute.parent() else {
        return Err(format!("invalid path {}", path.display()));
    };
    let parent = std::fs::canonicalize(parent)
        .map_err(|error| format!("resolve {}: {error}", path.display()))?;
    let file_name = absolute
        .file_name()
        .ok_or_else(|| format!("invalid path {}", path.display()))?;
    Ok(parent.join(file_name))
}

fn confirm_install(app_name: &str) -> Result<bool, String> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Ok(false);
    }
    eprint!("{app_name} is not installed in this workspace. Install it now? [Y/n] ");
    io::stderr()
        .flush()
        .map_err(|error| format!("show install prompt: {error}"))?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("read install answer: {error}"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    ))
}

fn caller_session_id() -> Option<String> {
    std::env::var("UNPEEL_SESSION_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "${UNPEEL_SESSION_ID}")
}

fn launch_editor(state: &serde_json::Value, resource: &ResolvedResource) -> Result<i32, String> {
    if resource.kind != "file" {
        return Err("The editor opener supports file resources only.".into());
    }
    let editor = state
        .get("code_editor")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("code");
    let status = Command::new(editor)
        .arg(&resource.id)
        .status()
        .map_err(|error| format!("run editor {editor:?}: {error}"))?;
    Ok(status.code().unwrap_or(1))
}

fn launch_system(resource: &ResolvedResource) -> Result<i32, String> {
    if resource.kind != "file" {
        return Err("The system opener supports file resources only.".into());
    }
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(command)
        .arg(&resource.id)
        .status()
        .map_err(|error| format!("run system opener {command:?}: {error}"))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_resolve() {
        let args = parse(&["README.md".into(), "--resolve".into(), "--json".into()]).unwrap();
        assert!(args.resolve);
        assert!(args.json);
        assert_eq!(args.resource, "README.md");
        assert!(!parse(&["README.md".into()]).unwrap().resolve);
    }

    #[test]
    fn parser_accepts_explicit_typed_resource() {
        let args = parse(&[
            "github:unpeel-com/unpeel#42".into(),
            "--kind".into(),
            "github.pull-request".into(),
            "--with".into(),
            "pull-requests".into(),
        ])
        .unwrap();
        assert_eq!(args.kind.as_deref(), Some("github.pull-request"));
        assert_eq!(args.app.as_deref(), Some("pull-requests"));
    }

    #[test]
    fn markdown_extension_is_registry_driven() {
        assert_eq!(
            resolve_file_media_type("/tmp/readme.markdown", None, false).unwrap(),
            Some("text/markdown".to_string())
        );
        // Unknown extensions are the editor's unless an App was named.
        assert_eq!(
            resolve_file_media_type("/tmp/notes.txt", None, false).unwrap(),
            None
        );
        assert!(resolve_file_media_type("/tmp/notes.txt", None, true).is_err());
        assert_eq!(
            resolve_file_media_type("/tmp/notes.txt", Some("text/plain"), true).unwrap(),
            Some("text/plain".to_string())
        );
    }

    #[test]
    fn working_tree_shortcut_is_a_typed_resource() {
        let args = Args {
            resource: "git:working-tree".into(),
            app: None,
            kind: None,
            media_type: None,
            json: false,
            resolve: false,
        };
        let resource = resolve_resource(&args).unwrap();
        assert_eq!(resource.kind, "git.working-tree");
        assert!(Path::new(&resource.id).is_absolute());
    }
}
