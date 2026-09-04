//! Git worktrees, frontend-agnostic — the Rust side of what
//! `WorktreeGit.swift` does, so the TUI and a headless host can create and
//! list the same worktrees the desktop does.
//!
//! Layout is a shared contract and must match byte for byte:
//! `~/.unpeel/worktrees/<repo-slug>-<fnv1a:08x>/<name-slug>`, where the hash
//! is FNV-1a over the repo's canonical toplevel path. New branches fork from
//! the mainline (`origin/HEAD`, else `origin/main`/`origin/master`, else a
//! local `main`/`master`), not from HEAD.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::app_paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    /// True when Unpeel created it (it lives under the worktrees root).
    pub managed: bool,
}

fn run_git(repo: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// FNV-1a over the UTF-8 bytes — same constants as the Swift port.
fn fnv1a(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Branch/dir slug: lowercase, non-alphanumerics collapsed to '-'.
pub fn slug(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `~/.unpeel/worktrees/<repo-slug>-<hash>` for a repo toplevel.
pub fn repo_worktrees_dir(toplevel: &str) -> PathBuf {
    let repo_name = Path::new(toplevel)
        .file_name()
        .map(|n| slug(&n.to_string_lossy()))
        .unwrap_or_else(|| "repo".into());
    // `{:08x}` zero-pads to 8 but keeps every significant digit.
    let hex = format!("{:08x}", fnv1a(toplevel));
    canonical_or_self(&app_paths::worktrees_root()).join(format!("{repo_name}-{hex}"))
}

/// The repo toplevel for any path inside a working tree.
pub fn repo_toplevel(path: &str) -> Result<String, String> {
    let top = run_git(path, &["rev-parse", "--show-toplevel"])?;
    Ok(canonical_or_self(Path::new(&top))
        .to_string_lossy()
        .to_string())
}

/// The ref new branches fork from: mainline if we can find one, else None
/// (meaning "fork from HEAD").
pub fn default_base_ref(repo: &str) -> Option<String> {
    if let Ok(head) = run_git(
        repo,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) {
        if !head.is_empty() {
            return Some(head);
        }
    }
    for candidate in ["origin/main", "origin/master"] {
        if run_git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{candidate}"),
            ],
        )
        .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    for candidate in ["main", "master"] {
        if run_git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{candidate}"),
            ],
        )
        .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Every worktree of the repo containing `path`, `managed` marking the ones
/// under Unpeel's worktrees root.
pub fn list(path: &str) -> Result<Vec<Worktree>, String> {
    let toplevel = repo_toplevel(path)?;
    let raw = run_git(&toplevel, &["worktree", "list", "--porcelain"])?;
    let managed_root = canonical_or_self(&app_paths::worktrees_root());
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                worktrees.push(entry);
            }
            let managed = Path::new(path).starts_with(&managed_root);
            current = Some(Worktree {
                path: path.to_string(),
                branch: None,
                managed,
            });
        } else if let Some(branch) = line.strip_prefix("branch ") {
            if let Some(entry) = current.as_mut() {
                entry.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
            }
        }
    }
    if let Some(entry) = current {
        worktrees.push(entry);
    }
    Ok(worktrees)
}

/// Create (or adopt) a worktree for `branch`. Returns its path. A best-effort
/// fetch freshens the mainline first, matching the desktop.
pub fn create(path: &str, branch: &str, base_ref: Option<&str>) -> Result<Worktree, String> {
    if branch.trim().is_empty() {
        return Err("a branch name is required".into());
    }
    if base_ref.is_some_and(|r| r.starts_with('-')) {
        return Err("invalid base ref".into());
    }
    let toplevel = repo_toplevel(path)?;
    let name = slug(branch);
    if name.is_empty() {
        return Err("branch name has no usable characters".into());
    }
    let dir = repo_worktrees_dir(&toplevel).join(&name);

    // Already there? Adopt it rather than failing.
    if dir.exists() {
        let existing = list(&toplevel)?
            .into_iter()
            .find(|w| Path::new(&w.path) == dir);
        if let Some(found) = existing {
            return Ok(found);
        }
    }
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;

    // Freshen the mainline; offline is fine.
    let _ = run_git(&toplevel, &["fetch", "--quiet", "origin"]);
    let base = base_ref
        .map(str::to_string)
        .or_else(|| default_base_ref(&toplevel));
    let dir_str = dir.to_string_lossy().to_string();

    let branch_exists = run_git(
        &toplevel,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    let mut args: Vec<String> = vec!["worktree".into(), "add".into()];
    if branch_exists {
        args.push(dir_str.clone());
        args.push(branch.to_string());
    } else {
        args.push("-b".into());
        args.push(branch.to_string());
        args.push(dir_str.clone());
        if let Some(base) = &base {
            args.push(base.clone());
        }
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git(&toplevel, &borrowed)?;
    Ok(Worktree {
        path: dir_str,
        branch: Some(branch.to_string()),
        managed: true,
    })
}

/// Remove a managed worktree. Refuses anything outside the worktrees root so
/// a stray call can't delete the user's main checkout.
pub fn remove(path: &str, force: bool) -> Result<(), String> {
    let managed_root = canonical_or_self(&app_paths::worktrees_root());
    let target = canonical_or_self(Path::new(path));
    if !target.starts_with(&managed_root) {
        return Err("refusing to remove a worktree Unpeel does not manage".into());
    }
    let toplevel = repo_toplevel(path)?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let target_str = target.to_string_lossy().to_string();
    args.push(&target_str);
    run_git(&toplevel, &args).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_slug_match_the_desktop_scheme() {
        // FNV-1a offset basis with no input.
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        // Known-answer for "a": basis ^ 'a' then * prime.
        let expected = (0xcbf2_9ce4_8422_2325u64 ^ 0x61).wrapping_mul(0x0000_0100_0000_01b3);
        assert_eq!(fnv1a("a"), expected);
        assert_eq!(slug("Feature/Big Fix"), "feature-big-fix");
        assert_eq!(slug("  --weird__name  "), "weird-name");
        // Trailing separators are trimmed, so a non-ascii tail just drops.
        assert_eq!(slug("já"), "j");
    }

    #[test]
    fn repo_dir_is_slug_plus_padded_hash() {
        let dir = repo_worktrees_dir("/Users/x/Dev/unpeel");
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let (repo, hex) = name.rsplit_once('-').expect("slug-hash");
        assert_eq!(repo, "unpeel");
        assert!(hex.len() >= 8, "hash zero-pads to at least 8: {hex}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn remove_refuses_unmanaged_paths() {
        let error = remove("/tmp", false).unwrap_err();
        assert!(error.contains("does not manage"), "{error}");
    }
}
