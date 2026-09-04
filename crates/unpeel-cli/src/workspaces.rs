//! Workspaces from the CLI — isolated Unpeel instances with their own state homes.
//!
//! The shared registry deliberately keeps its historical persistence contract:
//! one file at the REAL `~/.unpeel/profiles.json`, with a top-level `profiles`
//! array, and permanent homes under `~/.unpeel/profiles/<slug>`. Existing app
//! builds read that wire format, and provider hook configs bake absolute paths
//! to those homes. These legacy spellings are storage details, not product
//! vocabulary, and must not be migrated or removed casually.
//!
//! The registry never resolves through `app_paths::unpeel_home()`, which honors
//! `UNPEEL_HOME`: every instance must see the same registry. Writes are atomic
//! last-writer-wins, matching the app.
//!
//! `unpeel --workspace NAME …` claims the flag before any dispatch and sets
//! `UNPEEL_HOME` for the rest of the process; spawned hosts inherit the env,
//! so sessions, state, hook broadcasts, and pairing all stay in that home.

use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct WorkspaceInstanceRecord {
    pub id: String,
    pub name: String,
    /// Absolute path of the workspace's UNPEEL_HOME. Minted once at create;
    /// rename never moves it (hook configs may already point into it).
    pub home: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    /// Keys this build doesn't model survive a rewrite (compat with newer
    /// app/CLI versions editing the same file).
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct WorkspaceRegistryFile {
    version: i64,
    /// Historical wire key shared with existing native app builds.
    #[serde(rename = "profiles")]
    workspaces: Vec<WorkspaceInstanceRecord>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for WorkspaceRegistryFile {
    fn default() -> Self {
        Self {
            version: 1,
            workspaces: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

/// The real `~/.unpeel`, deliberately ignoring `UNPEEL_HOME`.
pub fn real_unpeel_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".unpeel")
}

/// Historical registry filename shared with existing native app builds.
fn registry_path(real_dir: &Path) -> PathBuf {
    real_dir.join("profiles.json")
}

/// Historical home root. Existing absolute homes and hook paths must remain
/// valid even though the user-facing concept is now called a workspace.
fn legacy_profiles_root(real_dir: &Path) -> PathBuf {
    real_dir.join("profiles")
}

fn load_file(real_dir: &Path) -> WorkspaceRegistryFile {
    std::fs::read(registry_path(real_dir))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

pub fn load(real_dir: &Path) -> Vec<WorkspaceInstanceRecord> {
    load_file(real_dir).workspaces
}

fn save_file(real_dir: &Path, file: &WorkspaceRegistryFile) -> Result<(), String> {
    let path = registry_path(real_dir);
    std::fs::create_dir_all(real_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(file).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())
}

/// The `--workspace` argument for a record: its home dir's basename.
pub fn slug_of(record: &WorkspaceInstanceRecord) -> String {
    Path::new(&record.home)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| record.home.clone())
}

fn trim_trailing_slash(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        path
    } else {
        trimmed
    }
}

/// Mirrors the native workspace registry's slugify: lowercase ASCII
/// alphanumerics, runs of anything else collapse to one dash, no
/// leading/trailing dashes.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true; // suppress leading dashes
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "workspace".into()
    } else {
        slug
    }
}

fn unique_slug(real_dir: &Path, name: &str, existing: &[WorkspaceInstanceRecord]) -> String {
    let base = slugify(name);
    let root = legacy_profiles_root(real_dir);
    let taken: std::collections::HashSet<String> = existing
        .iter()
        .map(|record| trim_trailing_slash(&record.home).to_string())
        .collect();
    let mut candidate = base.clone();
    let mut counter = 2;
    loop {
        let home = root.join(&candidate);
        let home_str = home.to_string_lossy().into_owned();
        if !taken.contains(trim_trailing_slash(&home_str)) && !home.exists() {
            return candidate;
        }
        candidate = format!("{base}-{counter}");
        counter += 1;
    }
}

pub fn create(real_dir: &Path, name: &str) -> Result<WorkspaceInstanceRecord, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("give the workspace a name".into());
    }
    // Re-read right before mutating: another instance may have edited the
    // registry (atomic last-writer-wins is the concurrency model).
    let mut file = load_file(real_dir);
    let slug = unique_slug(real_dir, trimmed, &file.workspaces);
    let home = legacy_profiles_root(real_dir).join(&slug);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let record = WorkspaceInstanceRecord {
        id: uuid::Uuid::new_v4().to_string().to_lowercase(),
        name: trimmed.to_string(),
        home: home.to_string_lossy().into_owned(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
        extra: serde_json::Map::new(),
    };
    file.workspaces.push(record.clone());
    save_file(real_dir, &file)?;
    Ok(record)
}

/// Find a workspace by exact name (case-insensitive) or slug. `Ok(None)` when
/// nothing matches; ambiguity is an error.
pub fn find(real_dir: &Path, reference: &str) -> Result<Option<WorkspaceInstanceRecord>, String> {
    let records = load(real_dir);
    let matches: Vec<&WorkspaceInstanceRecord> = records
        .iter()
        .filter(|r| r.name.eq_ignore_ascii_case(reference) || slug_of(r) == reference)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0].clone())),
        n => Err(format!("{n} workspaces match {reference:?} — use the slug")),
    }
}

fn not_found_message(real_dir: &Path, reference: &str) -> String {
    let known: Vec<String> = load(real_dir).iter().map(slug_of).collect();
    if known.is_empty() {
        format!(
            "no workspace named {reference:?} — create one with `unpeel workspaces add {reference}`"
        )
    } else {
        format!(
            "no workspace named {reference:?} (workspaces: {}) — create one with `unpeel workspaces add {reference}`",
            known.join(", ")
        )
    }
}

pub fn resolve(real_dir: &Path, reference: &str) -> Result<WorkspaceInstanceRecord, String> {
    find(real_dir, reference)?.ok_or_else(|| not_found_message(real_dir, reference))
}

/// Interactive fallback for `--workspace` misses: offer to create the workspace
/// on the spot. Only when talking to a human — piped/scripted invocations
/// keep the hard error instead of hanging on a prompt that can't be seen.
/// The prompt lives on stderr so a piped stdout (`--json`) stays clean.
fn offer_create(real_dir: &Path, reference: &str) -> Result<WorkspaceInstanceRecord, String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(not_found_message(real_dir, reference));
    }
    eprint!("no workspace named {reference:?} — create it? [y/N] ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
        let record = create(real_dir, reference)?;
        eprintln!("created {} → {}", record.name, record.home);
        Ok(record)
    } else {
        Err(not_found_message(real_dir, reference))
    }
}

/// Unregister a workspace. The home dir is always kept: hook configs may bake
/// absolute paths into it, and the app may still have it registered as data.
pub fn remove(real_dir: &Path, reference: &str) -> Result<WorkspaceInstanceRecord, String> {
    let removed = resolve(real_dir, reference)?;
    let mut file = load_file(real_dir);
    file.workspaces.retain(|r| r.id != removed.id);
    save_file(real_dir, &file)?;
    Ok(removed)
}

/// Claim `--workspace NAME` / `--workspace=NAME` out of the raw arg list,
/// before any command dispatch. Returns the workspace reference; the args are left
/// without the flag so the normal CLI/UI paths never see it.
///
/// The former `--profile` spelling is rejected explicitly. Letting it fall
/// through would make known one-shot commands silently run against the default
/// home, while other forms would unexpectedly open the interactive TUI.
/// Resolve the workspace this process is scoped to, for verbs that need the
/// registry slug back after `--workspace` re-homed the process (service unit
/// naming). No `UNPEEL_HOME` means the machine scope; a set home must be a
/// registered workspace so a unit can address it durably by slug.
pub fn current_scope() -> Result<Option<(String, PathBuf)>, String> {
    let Some(home) = std::env::var_os("UNPEEL_HOME").filter(|home| !home.is_empty()) else {
        return Ok(None);
    };
    let home = PathBuf::from(home);
    let canonical = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
    for record in load(&real_unpeel_dir()) {
        let recorded = PathBuf::from(trim_trailing_slash(&record.home));
        let recorded = std::fs::canonicalize(&recorded).unwrap_or(recorded);
        if recorded == canonical {
            return Ok(Some((slug_of(&record), PathBuf::from(&record.home))));
        }
    }
    Err(format!(
        "UNPEEL_HOME ({}) is not a registered workspace; register it with `unpeel workspaces add` first",
        home.display()
    ))
}

pub fn claim_workspace_flag(args: &mut Vec<String>) -> Result<Option<String>, String> {
    let mut found: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        if arg == "--profile" || arg.starts_with("--profile=") {
            return Err("`--profile` was renamed; use `--workspace`".into());
        }
        let value = if let Some(v) = arg.strip_prefix("--workspace=") {
            args.remove(index);
            Some(v.to_string())
        } else if arg == "--workspace" {
            args.remove(index);
            if index >= args.len() || args[index].starts_with("--") {
                return Err("--workspace requires a workspace name".into());
            }
            Some(args.remove(index))
        } else {
            index += 1;
            None
        };
        if let Some(value) = value {
            if value.is_empty() {
                return Err("--workspace requires a workspace name".into());
            }
            if found.is_some() {
                return Err("--workspace may be specified only once".into());
            }
            found = Some(value);
        }
    }
    Ok(found)
}

/// Resolve the claimed workspace and point this process (and every child it
/// spawns) at its home. An unknown name offers interactive creation.
pub fn enter(reference: &str) -> Result<(), String> {
    let real_dir = real_unpeel_dir();
    let record = match find(&real_dir, reference)? {
        Some(record) => record,
        None => offer_create(&real_dir, reference)?,
    };
    // The home was minted at create; recreate it if it vanished so a stale
    // registry entry degrades to an empty workspace instead of a crash.
    std::fs::create_dir_all(&record.home).map_err(|e| e.to_string())?;
    std::env::set_var("UNPEEL_HOME", &record.home);
    Ok(())
}

/// `unpeel workspaces [list | add <name> | remove <name>]`.
pub fn cli(args: &[String], json: bool) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let real_dir = real_unpeel_dir();
            let records = load(&real_dir);
            let active = std::env::var("UNPEEL_HOME").unwrap_or_default();
            let active = trim_trailing_slash(active.trim()).to_string();
            if json {
                let list: Vec<serde_json::Value> = records
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "name": r.name,
                            "slug": slug_of(r),
                            "home": r.home,
                            "active": !active.is_empty()
                                && trim_trailing_slash(&r.home) == active,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&list).unwrap_or_default()
                );
                return Ok(());
            }
            if records.is_empty() {
                println!("no workspaces — create one with `unpeel workspaces add <name>`");
                return Ok(());
            }
            for record in &records {
                let marker = if !active.is_empty() && trim_trailing_slash(&record.home) == active {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{marker} {:20} {:20} {}",
                    record.name,
                    slug_of(record),
                    record.home
                );
            }
            Ok(())
        }
        Some("add") | Some("create") => {
            let name = args[1..].join(" ");
            let record = create(&real_unpeel_dir(), &name)?;
            println!("created {} → {}", record.name, record.home);
            println!("run it with `unpeel --workspace {}`", slug_of(&record));
            Ok(())
        }
        Some("remove") | Some("rm") => {
            let Some(reference) = args.get(1) else {
                return Err("usage: unpeel workspaces remove <name>".into());
            };
            let record = remove(&real_unpeel_dir(), reference)?;
            println!(
                "removed {} from the registry — its data stays in {}",
                record.name, record.home
            );
            Ok(())
        }
        Some(other) => Err(format!("unknown workspaces subcommand: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_real_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join("unpeel-workspaces-test")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slugify_matches_app() {
        assert_eq!(slugify("Work Mac"), "work-mac");
        assert_eq!(slugify("  --Fancy!! Name-- "), "fancy-name");
        assert_eq!(slugify("æøå"), "workspace"); // non-ASCII drops entirely
        assert_eq!(slugify(""), "workspace");
        assert_eq!(slugify("A"), "a");
    }

    #[test]
    fn create_resolve_remove_roundtrip() {
        let dir = temp_real_dir();
        let record = create(&dir, "Work").unwrap();
        assert_eq!(slug_of(&record), "work");
        assert!(Path::new(&record.home).is_dir());

        // Same name mints a new slug, never reuses the home.
        let second = create(&dir, "Work").unwrap();
        assert_eq!(slug_of(&second), "work-2");

        assert_eq!(resolve(&dir, "work-2").unwrap().id, second.id);
        // Name lookup is ambiguous now; slug still works.
        assert!(resolve(&dir, "Work").is_err());
        assert!(resolve(&dir, "nope").is_err());
        // find(): a miss is Ok(None) (→ the create offer); ambiguity stays hard.
        assert!(find(&dir, "nope").unwrap().is_none());
        assert!(find(&dir, "work-2").unwrap().is_some());
        assert!(find(&dir, "Work").is_err());

        remove(&dir, "work-2").unwrap();
        assert_eq!(load(&dir).len(), 1);
        // Home dir survives removal (permanence rule).
        assert!(Path::new(&second.home).is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn registry_shape_matches_app_and_keeps_unknown_keys() {
        let dir = temp_real_dir();
        create(&dir, "One").unwrap();
        assert_eq!(registry_path(&dir), dir.join("profiles.json"));
        let raw = std::fs::read_to_string(registry_path(&dir)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["version"], 1);
        assert!(value.get("profiles").is_some());
        assert!(value.get("workspaces").is_none());
        let record = &value["profiles"][0];
        for key in ["id", "name", "home", "createdAt"] {
            assert!(record.get(key).is_some(), "missing {key}");
        }
        let legacy_home = record["home"].as_str().unwrap().to_string();
        assert!(legacy_home.starts_with(&format!(
            "{}/",
            legacy_profiles_root(&dir).to_string_lossy()
        )));

        // A newer writer's unknown keys must survive our rewrite.
        let mut value = value;
        value["futureTopLevel"] = serde_json::json!(true);
        value["profiles"][0]["futureField"] = serde_json::json!("keep");
        std::fs::write(registry_path(&dir), value.to_string()).unwrap();
        create(&dir, "Two").unwrap();
        let raw = std::fs::read_to_string(registry_path(&dir)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["futureTopLevel"], true);
        assert_eq!(value["profiles"][0]["futureField"], "keep");
        assert_eq!(value["profiles"][0]["home"], legacy_home);
        assert!(value.get("workspaces").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn claim_flag_forms() {
        let mut args: Vec<String> = ["--workspace", "work", "ls", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            claim_workspace_flag(&mut args).unwrap(),
            Some("work".to_string())
        );
        assert_eq!(args, ["ls", "--json"]);

        let mut args: Vec<String> = ["ls", "--workspace=work"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            claim_workspace_flag(&mut args).unwrap(),
            Some("work".to_string())
        );
        assert_eq!(args, ["ls"]);

        let mut args: Vec<String> = ["ls"].iter().map(|s| s.to_string()).collect();
        assert_eq!(claim_workspace_flag(&mut args).unwrap(), None);

        let mut args: Vec<String> = ["--workspace"].iter().map(|s| s.to_string()).collect();
        assert!(claim_workspace_flag(&mut args).is_err());
        let mut args: Vec<String> = ["--workspace", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(claim_workspace_flag(&mut args).is_err());
        let mut args: Vec<String> = ["--workspace=a", "--workspace", "b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(claim_workspace_flag(&mut args).is_err());
    }

    #[test]
    fn stale_profile_flags_are_rejected_without_mutating_args() {
        for raw in [
            vec!["--profile", "work", "ls"],
            vec!["ls", "--profile=work"],
        ] {
            let mut args: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
            let original = args.clone();
            let error = claim_workspace_flag(&mut args).unwrap_err();
            assert!(error.contains("use `--workspace`"), "{error}");
            assert_eq!(args, original);
        }
    }
}
