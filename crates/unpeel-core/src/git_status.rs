//! Uncommitted-change summary for a Session's working tree
//! (`session.git.status.read`): the `+N −M` a Controller shows in a pane
//! header before the user opens the Git App. Read-only and bounded:
//!
//! - one deadline covers the whole scan, well under the Controller's read
//!   timeout (a timed-out read drops the connection generation);
//! - every `git` call runs with `GIT_OPTIONAL_LOCKS=0` (never takes the index
//!   lock an agent's own `git` may need) and with the repository-configured
//!   command hooks a status read has no use for turned off (fsmonitor,
//!   textconv, external diff). Clean filters named by `.gitattributes` still
//!   run, as they do for any shell prompt that shows Git status;
//! - untracked files are counted up to fixed caps;
//! - a repository's answer is cached briefly per directory, and a directory
//!   outside Git much longer, so a shell in `~` does not spawn `git` on every
//!   poll;
//! - on macOS, the `/usr/bin/git` stub of a Mac without developer tools is
//!   never run (each run opens the install dialog).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Whole-scan budget: every `git` call and the untracked-file reads share it.
const SCAN_BUDGET: Duration = Duration::from_secs(4);
const REPOSITORY_CACHE_TTL: Duration = Duration::from_secs(2);
const OUTSIDE_GIT_CACHE_TTL: Duration = Duration::from_secs(30);
const CACHE_ENTRIES: usize = 64;
const MAX_UNTRACKED_FILES: usize = 500;
const MAX_UNTRACKED_FILE_BYTES: u64 = 1024 * 1024;
const MAX_UNTRACKED_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorkingTreeStatus {
    /// Absolute repository toplevel; the resource id a Controller hands to
    /// the Git App (`git.working-tree`).
    pub root: String,
    /// Current branch, `None` on a detached HEAD.
    pub branch: Option<String>,
    /// Changed paths: tracked changes against HEAD plus untracked files.
    pub files: u64,
    pub additions: u64,
    pub deletions: u64,
}

/// Summarize uncommitted changes for the working tree containing `path`.
/// `None` when `path` is not inside a Git working tree, or `git` is
/// missing, unusable, or too slow.
pub fn working_tree_status(path: &str) -> Option<GitWorkingTreeStatus> {
    type Cache = Mutex<HashMap<String, (Instant, Option<GitWorkingTreeStatus>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let fresh = |at: &Instant, status: &Option<GitWorkingTreeStatus>, now: Instant| {
        let ttl = if status.is_some() {
            REPOSITORY_CACHE_TTL
        } else {
            OUTSIDE_GIT_CACHE_TTL
        };
        now.duration_since(*at) < ttl
    };
    let cache = CACHE.get_or_init(Default::default);
    let now = Instant::now();
    {
        let cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, status)) = cache.get(path) {
            if fresh(at, status, now) {
                return status.clone();
            }
        }
    }
    let status = scan(path);
    let now = Instant::now();
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|_, (at, status)| fresh(at, status, now));
    if cache.len() < CACHE_ENTRIES {
        cache.insert(path.to_owned(), (now, status.clone()));
    }
    status
}

fn scan(path: &str) -> Option<GitWorkingTreeStatus> {
    if path.is_empty() || !Path::new(path).is_dir() || !git_is_usable() {
        return None;
    }
    let deadline = Instant::now() + SCAN_BUDGET;
    let root = run_git(path, &["rev-parse", "--show-toplevel"], deadline)?;
    let root = root.trim().to_owned();
    if root.is_empty() {
        return None;
    }
    let branch = run_git(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        deadline,
    )
    .map(|branch| branch.trim().to_owned())
    .filter(|branch| !branch.is_empty());
    // Before the first commit, diff against the empty tree so a first commit
    // in progress still shows its lines. Its id depends on the repository's
    // object format (SHA-1 or SHA-256), so ask rather than hardcode it.
    let base = match run_git(
        &root,
        &["rev-parse", "--quiet", "--verify", "HEAD"],
        deadline,
    ) {
        Some(_) => "HEAD".to_owned(),
        None => run_git(&root, &["hash-object", "-t", "tree", "/dev/null"], deadline)?
            .trim()
            .to_owned(),
    };
    let numstat = run_git(
        &root,
        &[
            "diff",
            &base,
            "--numstat",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "-z",
        ],
        deadline,
    )?;
    let (mut files, mut additions, deletions) = parse_numstat(&numstat);
    let untracked = run_git(
        &root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        deadline,
    )?;
    let untracked: Vec<&str> = untracked.split('\0').filter(|p| !p.is_empty()).collect();
    files += untracked.len() as u64;
    additions += count_untracked_lines(Path::new(&root), &untracked, deadline);
    Some(GitWorkingTreeStatus {
        root,
        branch,
        files,
        additions,
        deletions,
    })
}

/// `git diff --numstat -z` without renames: `<added>\t<deleted>\t<path>\0`
/// per file, `-\t-` for binary files (a changed file with no line counts).
fn parse_numstat(output: &str) -> (u64, u64, u64) {
    let (mut files, mut additions, mut deletions) = (0, 0, 0);
    for record in output.split('\0') {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(_path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        files += 1;
        additions += added.parse::<u64>().unwrap_or(0);
        deletions += deleted.parse::<u64>().unwrap_or(0);
    }
    (files, additions, deletions)
}

/// Lines in untracked text files, which `git diff` never reports. Bounded by
/// file count, bytes, and the scan deadline; binary files and oversized
/// files count as zero.
fn count_untracked_lines(root: &Path, paths: &[&str], deadline: Instant) -> u64 {
    let mut total_bytes = 0u64;
    let mut lines = 0u64;
    for relative in paths.iter().take(MAX_UNTRACKED_FILES) {
        if Instant::now() >= deadline {
            break;
        }
        let path = root.join(relative);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_UNTRACKED_FILE_BYTES {
            continue;
        }
        total_bytes += metadata.len();
        if total_bytes > MAX_UNTRACKED_TOTAL_BYTES {
            break;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
            continue;
        }
        lines += text_line_count(&bytes);
    }
    lines
}

/// Lines as Git counts them: a final line without a newline still counts.
fn text_line_count(bytes: &[u8]) -> u64 {
    let newlines = bytes.iter().filter(|&&byte| byte == b'\n').count() as u64;
    if bytes.last().is_some_and(|&byte| byte != b'\n') {
        newlines + 1
    } else {
        newlines
    }
}

/// Whether running `git` is safe and useful here, decided once per process.
/// On macOS `/usr/bin/git` is a stub that opens the developer-tools install
/// dialog when no toolchain is selected; `xcode-select -p` reports that
/// without prompting. Any other `git` on `PATH` is used as found.
fn git_is_usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        if !cfg!(target_os = "macos") {
            return true;
        }
        let resolved = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|dir| dir.join("git"))
            .find(|candidate| candidate.is_file());
        match resolved {
            Some(git) if git != Path::new("/usr/bin/git") => true,
            Some(_) => Command::new("/usr/bin/xcode-select")
                .arg("-p")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success()),
            None => false,
        }
    })
}

/// Run `git -C <dir> <args>` until `deadline`; stdout on success, `None` on
/// failure, timeout, or a missing `git`.
fn run_git(dir: &str, args: &[&str], deadline: Instant) -> Option<String> {
    if Instant::now() >= deadline {
        return None;
    }
    let mut child = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout on its own thread so a large diff can't fill the pipe and
    // stall the child. The result comes back over a channel with the same
    // deadline: a helper process that inherited the pipe can keep it open
    // after `git` exits, and that must strand only this detached reader,
    // never the request thread.
    let mut stdout = child.stdout.take()?;
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = output_tx.send(stdout.read_to_end(&mut buffer).ok().map(|_| buffer));
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            // The child is ours and has not been reaped, so its pid is still
            // reserved for it: this kill cannot reach another process.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    let output = output_rx.recv_timeout(remaining).ok()??;
    status
        .success()
        .then(|| String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        dir
    }

    fn root_of(dir: &tempfile::TempDir) -> String {
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn numstat_counts_text_and_binary_files() {
        let output = "3\t1\ta.txt\0-\t-\timage.png\0\0";
        assert_eq!(parse_numstat(output), (2, 3, 1));
        assert_eq!(parse_numstat(""), (0, 0, 0));
    }

    #[test]
    fn line_count_matches_git_for_a_missing_final_newline() {
        assert_eq!(text_line_count(b""), 0);
        assert_eq!(text_line_count(b"a\nb\n"), 2);
        assert_eq!(text_line_count(b"a\nb"), 2);
    }

    #[test]
    fn a_directory_outside_git_has_no_status() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scan(&dir.path().to_string_lossy()), None);
        assert_eq!(scan(""), None);
        assert_eq!(scan("/definitely/not/a/dir"), None);
    }

    #[test]
    fn a_repository_before_its_first_commit_counts_staged_and_untracked() {
        let dir = repo();
        std::fs::write(dir.path().join("staged.txt"), "one\ntwo\n").unwrap();
        git(dir.path(), &["add", "staged.txt"]);
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let status = scan(&dir.path().to_string_lossy()).unwrap();
        assert_eq!(status.root, root_of(&dir));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(
            (status.files, status.additions, status.deletions),
            (2, 3, 0)
        );
    }

    #[test]
    fn tracked_edits_and_untracked_files_add_up() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        let clean = scan(&dir.path().to_string_lossy()).unwrap();
        assert_eq!((clean.files, clean.additions, clean.deletions), (0, 0, 0));

        std::fs::write(dir.path().join("a.txt"), "1\nchanged\n3\n4\n").unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 9, 9, 9]).unwrap();
        std::fs::write(dir.path().join("untracked.md"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0u8, 1]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let status = scan(&dir.path().join("sub").to_string_lossy()).unwrap();
        assert_eq!(status.root, root_of(&dir));
        // a.txt +2 −1, bin.dat binary, untracked.md +3, blob.bin binary.
        assert_eq!(
            (status.files, status.additions, status.deletions),
            (4, 5, 1)
        );
    }

    #[test]
    fn an_expired_deadline_runs_nothing() {
        let dir = repo();
        let past = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            run_git(
                &dir.path().to_string_lossy(),
                &["rev-parse", "--show-toplevel"],
                past
            ),
            None
        );
    }

    #[test]
    fn a_sha256_repository_before_its_first_commit_still_counts() {
        let dir = tempfile::tempdir().unwrap();
        git(
            dir.path(),
            &["init", "-q", "-b", "main", "--object-format=sha256"],
        );
        std::fs::write(dir.path().join("staged.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "staged.txt"]);
        let status = scan(&dir.path().to_string_lossy()).unwrap();
        assert_eq!((status.files, status.additions), (1, 1));
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "1\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        git(dir.path(), &["checkout", "-q", "--detach"]);
        let status = scan(&dir.path().to_string_lossy()).unwrap();
        assert_eq!(status.branch, None);
    }
}
