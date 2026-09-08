use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    path: PathBuf,
    previous_path: Option<PathBuf>,
    index_status: char,
    worktree_status: char,
}

impl ChangedFile {
    #[cfg(test)]
    pub(crate) fn fixture(path: impl Into<PathBuf>, index: char, worktree: char) -> Self {
        Self {
            path: path.into(),
            previous_path: None,
            index_status: index,
            worktree_status: worktree,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    #[cfg(test)]
    pub fn display_path(&self) -> String {
        self.previous_path.as_ref().map_or_else(
            || self.path.display().to_string(),
            |previous| format!("{} → {}", previous.display(), self.path.display()),
        )
    }

    #[must_use]
    pub fn list_name(&self) -> String {
        let current = file_name(&self.path);
        self.previous_path.as_ref().map_or_else(
            || current.clone(),
            |previous| {
                let previous = file_name(previous);
                if previous == current {
                    current.clone()
                } else {
                    format!("{previous} → {current}")
                }
            },
        )
    }

    #[must_use]
    pub fn status_symbol(&self) -> char {
        if self.is_conflicted() {
            'U'
        } else if self.is_untracked() {
            '?'
        } else if self.index_status == 'R' || self.worktree_status == 'R' {
            'R'
        } else if self.index_status == 'C' || self.worktree_status == 'C' {
            'C'
        } else if self.index_status == 'A' || self.worktree_status == 'A' {
            'A'
        } else if self.index_status == 'D' || self.worktree_status == 'D' {
            'D'
        } else if self.index_status == 'T' || self.worktree_status == 'T' {
            'T'
        } else {
            'M'
        }
    }

    #[must_use]
    pub fn state_label(&self) -> &'static str {
        if self.is_conflicted() {
            "conflict"
        } else if self.is_untracked() {
            "untracked"
        } else {
            match (self.index_status != ' ', self.worktree_status != ' ') {
                (true, true) => "staged + unstaged",
                (true, false) => "staged",
                (false, true) => "unstaged",
                (false, false) => "changed",
            }
        }
    }

    #[must_use]
    pub fn is_untracked(&self) -> bool {
        self.index_status == '?' && self.worktree_status == '?'
    }

    fn is_conflicted(&self) -> bool {
        matches!(
            (self.index_status, self.worktree_status),
            ('D', 'D')
                | ('A', 'U')
                | ('U', 'D')
                | ('U', 'A')
                | ('D', 'U')
                | ('A', 'A')
                | ('U', 'U')
        )
    }

    fn pathspecs(&self) -> impl Iterator<Item = &Path> {
        self.previous_path
            .iter()
            .map(PathBuf::as_path)
            .chain(std::iter::once(self.path.as_path()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffDocument {
    pub file: ChangedFile,
    pub lines: Vec<String>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub short_id: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

impl DiffDocument {
    fn from_text(file: ChangedFile, text: String) -> Self {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let additions = lines
            .iter()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
            .count();
        let deletions = lines
            .iter()
            .filter(|line| line.starts_with('-') && !line.starts_with("---"))
            .count();
        Self {
            file,
            lines,
            additions,
            deletions,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAction {
    Fetch,
    Pull,
    Push,
}

impl RemoteAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch",
            Self::Pull => "Pull",
            Self::Push => "Push",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteState {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub remote_ref: Option<String>,
    pub ahead: usize,
    pub behind: usize,
}

impl RemoteState {
    pub fn allows(&self, action: RemoteAction) -> bool {
        match action {
            RemoteAction::Fetch => self.remote.is_some(),
            RemoteAction::Pull => {
                self.remote.is_some()
                    && self.upstream.is_some()
                    && self.head.is_some()
                    && !(self.ahead > 0 && self.behind > 0)
            }
            RemoteAction::Push => {
                self.remote.is_some()
                    && self.remote_ref.is_some()
                    && self.head.is_some()
                    && self.behind == 0
            }
        }
    }

    pub fn primary(&self) -> RemoteAction {
        if self.behind > 0 && self.ahead == 0 {
            RemoteAction::Pull
        } else if self.ahead > 0 && self.behind == 0 {
            RemoteAction::Push
        } else {
            RemoteAction::Fetch
        }
    }
}

#[derive(Clone, Debug)]
pub struct Repository {
    root: PathBuf,
}

impl Repository {
    pub fn discover(start: impl AsRef<Path>) -> io::Result<Self> {
        let mut start = start.as_ref().to_path_buf();
        if start.is_file() {
            start.pop();
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(&start)
            .args(["rev-parse", "--show-toplevel"])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()?;
        if !output.status.success() {
            return Err(git_error(
                &output,
                format!("{} is not inside a Git repository", start.display()),
            ));
        }
        let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if root.is_empty() {
            return Err(io::Error::other("Git returned an empty repository root"));
        }
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The checked-out branch, or the short commit id when HEAD is detached.
    /// Includes the branch name before the first commit.
    pub fn branch(&self) -> Option<String> {
        let output = self
            .git(["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && !name.is_empty() {
            return Some(name);
        }
        let output = self.git(["rev-parse", "--short", "HEAD"]).ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|short| !short.is_empty())
    }

    fn optional_text(&self, args: &[&str]) -> io::Result<Option<String>> {
        let output = self.git(args)?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|value| !value.is_empty()))
    }

    pub fn remote_state(&self) -> io::Result<RemoteState> {
        let branch = self.optional_text(&["symbolic-ref", "--quiet", "HEAD"])?;
        let head = self.optional_text(&["rev-parse", "--verify", "HEAD"])?;
        let mut state = RemoteState {
            branch,
            head,
            ..RemoteState::default()
        };
        if let Some(branch) = &state.branch
            && let Some(tracking) = self.optional_text(&[
                "for-each-ref",
                "--format=%(upstream)%00%(upstream:remotename)%00%(upstream:remoteref)",
                branch,
            ])?
        {
            let fields: Vec<_> = tracking.split('\0').collect();
            if fields.len() == 3 && !fields[0].is_empty() && fields[1] != "." {
                state.upstream = Some(fields[0].to_owned());
                state.remote = Some(fields[1].to_owned());
                state.remote_ref = Some(fields[2].to_owned());
                if let Some(counts) = self.optional_text(&[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("HEAD...{}", fields[0]),
                    "--",
                ])? {
                    let mut counts = counts.split_whitespace();
                    state.ahead = counts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    state.behind = counts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                }
            }
        }
        if state.remote.is_none()
            && let Some(remotes) = self.optional_text(&["remote"])?
        {
            state.remote = remotes
                .lines()
                .find(|name| *name == "origin")
                .or_else(|| remotes.lines().next())
                .map(str::to_owned);
        }
        Ok(state)
    }

    /// Invoked by a background worker only after an explicit UI action.
    /// Push names an exact commit/ref; pull checks the checkout again after fetch.
    pub fn remote_action(&self, action: RemoteAction, expected: &RemoteState) -> io::Result<()> {
        if !expected.allows(action) {
            return Err(io::Error::other(
                "This action needs a remote and a tracking branch; diverged branches must be resolved first",
            ));
        }
        let remote = expected.remote.as_deref().expect("validated remote");
        if self.remote_state()? != *expected {
            return Err(io::Error::other(
                "Repository changed. Refresh and try again",
            ));
        }
        match action {
            RemoteAction::Fetch => self.remote_command(&["fetch", "--", remote]),
            RemoteAction::Pull => {
                self.remote_command(&["fetch", "--", remote])?;
                let current = self.remote_state()?;
                if current.branch != expected.branch
                    || current.head != expected.head
                    || current.upstream != expected.upstream
                    || current.remote != expected.remote
                {
                    return Err(io::Error::other(
                        "Checkout changed during fetch. Pull again from the current branch",
                    ));
                }
                let upstream = expected.upstream.as_deref().expect("validated upstream");
                let target = self
                    .optional_text(&["rev-parse", "--verify", upstream])?
                    .ok_or_else(|| io::Error::other("Upstream branch is unavailable"))?;
                self.remote_command(&["merge", "--ff-only", "--no-autostash", &target])
            }
            RemoteAction::Push => {
                let refspec = format!(
                    "{}:{}",
                    expected.head.as_deref().expect("validated HEAD"),
                    expected
                        .remote_ref
                        .as_deref()
                        .expect("validated remote ref")
                );
                self.remote_command(&["push", "--no-force", "--no-mirror", "--", remote, &refspec])
            }
        }
    }

    fn remote_command(&self, args: &[&str]) -> io::Result<()> {
        let output = self
            .git_command()
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr
                .lines()
                .find(|line| line.starts_with("fatal:") || line.starts_with("error:"))
                .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))
                .unwrap_or("Git remote operation failed");
            Err(io::Error::other(message.to_owned()))
        }
    }

    pub fn changed_files(&self) -> io::Result<Vec<ChangedFile>> {
        let output = self.git(["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        if !output.status.success() {
            return Err(git_error(&output, "Unable to read Git status"));
        }
        let records: Vec<&[u8]> = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .collect();
        let mut files = Vec::new();
        let mut cursor = 0;
        while let Some(record) = records.get(cursor).copied() {
            cursor += 1;
            if record.len() < 4 || record[2] != b' ' {
                continue;
            }
            let index_status = char::from(record[0]);
            let worktree_status = char::from(record[1]);
            if index_status == '!' && worktree_status == '!' {
                continue;
            }
            let path = decode_path(&record[3..]);
            let has_previous =
                matches!(index_status, 'R' | 'C') || matches!(worktree_status, 'R' | 'C');
            let previous_path = has_previous.then(|| {
                let previous = records.get(cursor).copied().unwrap_or_default();
                cursor = cursor.saturating_add(1);
                decode_path(previous)
            });
            files.push(ChangedFile {
                path,
                previous_path,
                index_status,
                worktree_status,
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }

    pub fn history(&self, limit: usize) -> io::Result<Vec<Commit>> {
        let head = self.git(["rev-parse", "--verify", "--quiet", "HEAD"])?;
        if head.status.code() == Some(1) && head.stdout.is_empty() {
            return Ok(Vec::new());
        }
        if !head.status.success() {
            return Err(git_error(&head, "Unable to read HEAD"));
        }
        let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        let output = self.git([
            "log",
            "--no-color",
            "--no-show-signature",
            "-z",
            "--format=%H%x00%h%x00%an%x00%as%x00%s",
            &format!("--max-count={limit}"),
            &head,
            "--",
        ])?;
        if !output.status.success() {
            return Err(git_error(&output, "Unable to read commit history"));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let fields: Vec<&str> = text.split_terminator('\0').collect();
        if !fields.len().is_multiple_of(5) {
            return Err(io::Error::other("Git returned an incomplete commit record"));
        }
        Ok(fields
            .chunks_exact(5)
            .map(|fields| Commit {
                id: fields[0].into(),
                short_id: fields[1].into(),
                author: fields[2].into(),
                date: fields[3].into(),
                subject: fields[4].into(),
            })
            .collect())
    }

    /// Compare root commits to the empty tree and merges to their first parent.
    /// Both the file list and patches use exactly this comparison.
    fn commit_command(&self, commit: &Commit) -> io::Result<Command> {
        if !matches!(commit.id.len(), 40 | 64) || !commit.id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(io::Error::other("Invalid commit id"));
        }
        // An explicit first-parent comparison also works on Git 2.30
        // (Debian 11), before --diff-merges=first-parent was available.
        let parents = self.git(["rev-list", "--parents", "--max-count=1", &commit.id, "--"])?;
        if !parents.status.success() {
            return Err(git_error(&parents, "Unable to read commit parents"));
        }
        let parents = String::from_utf8_lossy(&parents.stdout);
        let mut command = self.git_command();
        command.args([
            "diff-tree",
            "--root",
            "--no-commit-id",
            "-r",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
        ]);
        if let Some(parent) = parents.split_whitespace().nth(1) {
            command.arg(parent);
        }
        Ok(command)
    }

    pub fn commit_files(&self, commit: &Commit) -> io::Result<Vec<ChangedFile>> {
        let output = self
            .commit_command(commit)?
            .args(["--name-status", "-z", &commit.id, "--"])
            .output()?;
        if !output.status.success() {
            return Err(git_error(&output, "Unable to read commit files"));
        }
        let mut fields = output.stdout.split(|byte| *byte == 0);
        let mut files = Vec::new();
        while let Some(status) = fields.next().filter(|status| !status.is_empty()) {
            let symbol = char::from(status[0]);
            let path = fields
                .next()
                .ok_or_else(|| io::Error::other("Missing commit file path"))?;
            let (previous_path, path) = if matches!(symbol, 'R' | 'C') {
                (
                    Some(decode_path(path)),
                    fields
                        .next()
                        .ok_or_else(|| io::Error::other("Missing renamed file path"))?,
                )
            } else {
                (None, path)
            };
            files.push(ChangedFile {
                path: decode_path(path),
                previous_path,
                index_status: symbol,
                worktree_status: ' ',
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(files)
    }

    pub fn commit_diff(&self, commit: &Commit, file: &ChangedFile) -> io::Result<DiffDocument> {
        let output = self
            .commit_command(commit)?
            .args(["-p", &commit.id, "--"])
            .args(file.pathspecs())
            .output()?;
        if !output.status.success() {
            return Err(git_error(&output, "Unable to read commit diff"));
        }
        Ok(DiffDocument::from_text(
            file.clone(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    pub fn diff(&self, file: &ChangedFile) -> io::Result<DiffDocument> {
        let text = if file.is_untracked() {
            self.untracked_diff(file)?
        } else {
            self.tracked_diff(file)?
        };
        Ok(DiffDocument::from_text(file.clone(), text))
    }

    fn tracked_diff(&self, file: &ChangedFile) -> io::Result<String> {
        let mut command = self.git_command();
        command.args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--find-renames",
            "HEAD",
            "--",
        ]);
        command.args(file.pathspecs());
        let output = command.output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        // An unborn branch has no HEAD. Preserve the same useful view by
        // composing its index and worktree patches.
        let staged = self.diff_phase(file, true)?;
        let unstaged = self.diff_phase(file, false)?;
        if staged.is_empty() && unstaged.is_empty() {
            return Err(git_error(&output, "Unable to read file diff"));
        }
        Ok(join_patches(staged, unstaged))
    }

    fn diff_phase(&self, file: &ChangedFile, cached: bool) -> io::Result<String> {
        let mut command = self.git_command();
        command.args(["diff", "--no-color", "--no-ext-diff", "--find-renames"]);
        if cached {
            command.arg("--cached");
        }
        command.arg("--").args(file.pathspecs());
        let output = command.output()?;
        if !output.status.success() {
            return Err(git_error(&output, "Unable to read file diff"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn untracked_diff(&self, file: &ChangedFile) -> io::Result<String> {
        let output = self
            .git_command()
            .args([
                OsStr::new("diff"),
                OsStr::new("--no-index"),
                OsStr::new("--no-color"),
                OsStr::new("--src-prefix=a/"),
                OsStr::new("--dst-prefix=b/"),
                OsStr::new("--"),
                OsStr::new("/dev/null"),
                file.path().as_os_str(),
            ])
            .output()?;
        if output.status.success() || output.status.code() == Some(1) {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(git_error(&output, "Unable to read untracked file"))
    }

    fn git<I, S>(&self, arguments: I) -> io::Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.git_command().args(arguments).output()
    }

    fn git_command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.root)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0");
        command
    }
}

fn decode_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn join_patches(staged: String, unstaged: String) -> String {
    match (staged.is_empty(), unstaged.is_empty()) {
        (true, true) => String::new(),
        (false, true) => staged,
        (true, false) => unstaged,
        (false, false) => format!("{staged}\n{unstaged}"),
    }
}

fn git_error(output: &Output, fallback: impl Into<String>) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    io::Error::other(if stderr.is_empty() {
        fallback.into()
    } else {
        stderr
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> (tempfile::TempDir, Repository) {
        let directory = tempfile::tempdir().unwrap();
        run_git(directory.path(), &["init", "-b", "main"]);
        run_git(directory.path(), &["config", "user.name", "Unpeel Tests"]);
        run_git(
            directory.path(),
            &["config", "user.email", "tests@unpeel.local"],
        );
        std::fs::write(directory.path().join("kept.txt"), "before\n").unwrap();
        std::fs::write(directory.path().join("removed.txt"), "remove me\n").unwrap();
        run_git(directory.path(), &["add", "."]);
        run_git(directory.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(directory.path()).unwrap();
        (directory, repository)
    }

    fn tracking_repository() -> (tempfile::TempDir, Repository, tempfile::TempDir) {
        let (directory, repo) = repository();
        let remote = tempfile::tempdir().unwrap();
        run_git(remote.path(), &["init", "--bare", "-b", "main"]);
        run_git(
            directory.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        run_git(directory.path(), &["push", "-u", "origin", "main"]);
        (directory, repo, remote)
    }

    fn commit_file(root: &Path, name: &str, text: &str) {
        std::fs::write(root.join(name), text).unwrap();
        run_git(root, &["add", name]);
        run_git(root, &["commit", "-m", text]);
    }

    fn clone_remote(remote: &Path) -> tempfile::TempDir {
        let clone = tempfile::tempdir().unwrap();
        run_git(clone.path(), &["clone", remote.to_str().unwrap(), "."]);
        run_git(clone.path(), &["config", "user.name", "Unpeel Tests"]);
        run_git(
            clone.path(),
            &["config", "user.email", "tests@unpeel.local"],
        );
        clone
    }

    #[test]
    fn remote_actions_follow_upstream_and_fast_forward_without_merges() {
        let (directory, repo, remote) = tracking_repository();
        let state = repo.remote_state().unwrap();
        assert_eq!(state.primary(), RemoteAction::Fetch);
        assert_eq!(state.remote.as_deref(), Some("origin"));
        commit_file(directory.path(), "local.txt", "local commit");
        let state = repo.remote_state().unwrap();
        assert_eq!(
            (state.ahead, state.behind, state.primary()),
            (1, 0, RemoteAction::Push)
        );
        repo.remote_action(RemoteAction::Push, &state).unwrap();
        assert_eq!(repo.remote_state().unwrap().ahead, 0);
        let other = clone_remote(remote.path());
        commit_file(other.path(), "remote.txt", "remote commit");
        run_git(other.path(), &["push"]);
        repo.remote_action(RemoteAction::Fetch, &repo.remote_state().unwrap())
            .unwrap();
        let state = repo.remote_state().unwrap();
        assert_eq!(
            (state.ahead, state.behind, state.primary()),
            (0, 1, RemoteAction::Pull)
        );
        repo.remote_action(RemoteAction::Pull, &state).unwrap();
        assert_eq!(repo.remote_state().unwrap().behind, 0);
        assert_eq!(
            std::fs::read_to_string(directory.path().join("remote.txt")).unwrap(),
            "remote commit"
        );
        assert_eq!(repo.history(10).unwrap().len(), 3);
    }

    #[test]
    fn pull_preserves_dirty_files_and_rejects_divergence_found_during_fetch() {
        let (directory, repo, remote) = tracking_repository();
        let other = clone_remote(remote.path());
        commit_file(other.path(), "kept.txt", "remote edit");
        run_git(other.path(), &["push"]);
        std::fs::write(directory.path().join("kept.txt"), "uncommitted edit").unwrap();
        let state = repo.remote_state().unwrap();
        assert!(repo.remote_action(RemoteAction::Pull, &state).is_err());
        assert_eq!(repo.remote_state().unwrap().head, state.head);
        assert_eq!(
            std::fs::read_to_string(directory.path().join("kept.txt")).unwrap(),
            "uncommitted edit"
        );
        // Reset only this throwaway test checkout to simulate a fresh, stale tracking ref.
        run_git(directory.path(), &["restore", "kept.txt"]);
        run_git(
            directory.path(),
            &[
                "update-ref",
                "refs/remotes/origin/main",
                state.head.as_deref().unwrap(),
            ],
        );
        commit_file(directory.path(), "local.txt", "local commit");
        let stale = repo.remote_state().unwrap();
        assert_eq!(stale.behind, 0);
        assert!(repo.remote_action(RemoteAction::Pull, &stale).is_err());
        let current = repo.remote_state().unwrap();
        assert_eq!(current.head, stale.head);
        assert_eq!((current.ahead, current.behind), (1, 1));
        assert!(!current.allows(RemoteAction::Push));
        assert!(!current.allows(RemoteAction::Pull));
    }

    #[test]
    fn remote_actions_reject_stale_checkouts_and_handle_missing_upstream() {
        let (directory, repo, _remote) = tracking_repository();
        let stale = repo.remote_state().unwrap();
        run_git(directory.path(), &["checkout", "-b", "another"]);
        assert!(repo.remote_action(RemoteAction::Push, &stale).is_err());
        let state = repo.remote_state().unwrap();
        assert!(state.allows(RemoteAction::Fetch));
        assert!(!state.allows(RemoteAction::Push));
        assert!(!state.allows(RemoteAction::Pull));
        run_git(directory.path(), &["checkout", "--detach"]);
        let detached = repo.remote_state().unwrap();
        assert!(detached.allows(RemoteAction::Fetch));
        assert!(!detached.allows(RemoteAction::Push));
        run_git(directory.path(), &["remote", "remove", "origin"]);
        assert!(!repo.remote_state().unwrap().allows(RemoteAction::Fetch));
    }

    #[test]
    fn discovers_the_root_from_a_nested_folder() {
        let (directory, repository) = repository();
        let nested = directory.path().join("one/two");
        std::fs::create_dir_all(&nested).unwrap();
        let nested_repository = Repository::discover(nested).unwrap();
        assert_eq!(nested_repository.root(), repository.root());
    }

    #[test]
    fn reports_tracked_deleted_and_untracked_files() {
        let (directory, repository) = repository();
        std::fs::write(directory.path().join("kept.txt"), "after\n").unwrap();
        std::fs::remove_file(directory.path().join("removed.txt")).unwrap();
        std::fs::write(directory.path().join("new.txt"), "new\n").unwrap();

        let files = repository.changed_files().unwrap();
        assert_eq!(files.len(), 3);
        assert!(
            files.iter().any(|file| {
                file.path() == Path::new("kept.txt") && file.status_symbol() == 'M'
            })
        );
        assert!(files.iter().any(|file| {
            file.path() == Path::new("removed.txt") && file.status_symbol() == 'D'
        }));
        assert!(files.iter().any(|file| {
            file.path() == Path::new("new.txt") && file.state_label() == "untracked"
        }));
    }

    #[test]
    fn produces_unified_diffs_and_counts_changed_lines() {
        let (directory, repository) = repository();
        std::fs::write(directory.path().join("kept.txt"), "before\nafter\n").unwrap();
        std::fs::write(directory.path().join("new.txt"), "one\ntwo\n").unwrap();
        let files = repository.changed_files().unwrap();

        let tracked = files
            .iter()
            .find(|file| file.path() == Path::new("kept.txt"))
            .unwrap();
        let tracked_diff = repository.diff(tracked).unwrap();
        assert!(tracked_diff.lines.iter().any(|line| line == "+after"));
        assert_eq!(tracked_diff.additions, 1);

        let untracked = files
            .iter()
            .find(|file| file.path() == Path::new("new.txt"))
            .unwrap();
        let untracked_diff = repository.diff(untracked).unwrap();
        assert_eq!(untracked_diff.additions, 2);
        assert_eq!(untracked_diff.deletions, 0);
    }

    #[test]
    fn history_and_root_patches_ignore_the_working_tree() {
        let (directory, repository) = repository();
        std::fs::write(directory.path().join("kept.txt"), "uncommitted\n").unwrap();
        let commits = repository.history(10).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "initial");
        assert_eq!(commits[0].author, "Unpeel Tests");
        let files = repository.commit_files(&commits[0]).unwrap();
        assert_eq!(files.len(), 2);
        let patch = repository.commit_diff(&commits[0], &files[0]).unwrap();
        assert!(patch.lines.iter().any(|line| line == "+before"));
        assert!(!patch.lines.iter().any(|line| line.contains("uncommitted")));
    }

    #[test]
    fn historical_renames_and_literal_paths_are_preserved() {
        let (directory, repository) = repository();
        run_git(directory.path(), &["mv", "kept.txt", "renamed\nfile.txt"]);
        std::fs::write(directory.path().join("[a].txt"), "literal\n").unwrap();
        std::fs::write(directory.path().join("a.txt"), "other\n").unwrap();
        run_git(directory.path(), &["add", "."]);
        run_git(directory.path(), &["commit", "-m", "Rename and add files"]);
        let commit = repository.history(1).unwrap().remove(0);
        let files = repository.commit_files(&commit).unwrap();
        let renamed = files
            .iter()
            .find(|file| file.status_symbol() == 'R')
            .unwrap();
        assert_eq!(renamed.path(), Path::new("renamed\nfile.txt"));
        assert_eq!(
            renamed.previous_path.as_deref(),
            Some(Path::new("kept.txt"))
        );
        assert!(
            repository
                .commit_diff(&commit, renamed)
                .unwrap()
                .lines
                .iter()
                .any(|line| line.starts_with("rename from "))
        );
        let literal = files
            .iter()
            .find(|file| file.path() == Path::new("[a].txt"))
            .unwrap();
        let patch = repository.commit_diff(&commit, literal).unwrap();
        assert_eq!(patch.additions, 1);
        assert!(patch.lines.iter().any(|line| line == "+literal"));
        assert!(!patch.lines.iter().any(|line| line == "+other"));
    }

    #[test]
    fn merge_history_uses_the_first_parent_for_files_and_patches() {
        let (directory, repository) = repository();
        run_git(directory.path(), &["checkout", "-b", "feature"]);
        std::fs::write(directory.path().join("feature.txt"), "feature\n").unwrap();
        run_git(directory.path(), &["add", "."]);
        run_git(directory.path(), &["commit", "-m", "Feature"]);
        run_git(directory.path(), &["checkout", "main"]);
        std::fs::write(directory.path().join("main.txt"), "main\n").unwrap();
        run_git(directory.path(), &["add", "."]);
        run_git(directory.path(), &["commit", "-m", "Main"]);
        run_git(
            directory.path(),
            &["merge", "--no-ff", "feature", "-m", "Merge feature"],
        );
        let commit = repository.history(1).unwrap().remove(0);
        let files = repository.commit_files(&commit).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path(), Path::new("feature.txt"));
        assert!(
            repository
                .commit_diff(&commit, &files[0])
                .unwrap()
                .lines
                .iter()
                .any(|line| line == "+feature")
        );
        assert_eq!(repository.history(2).unwrap().len(), 2);
    }

    #[test]
    fn empty_history_and_detached_head_are_supported() {
        let directory = tempfile::tempdir().unwrap();
        run_git(directory.path(), &["init", "-b", "fresh"]);
        let empty = Repository::discover(directory.path()).unwrap();
        assert!(empty.history(100).unwrap().is_empty());
        assert_eq!(empty.branch().as_deref(), Some("fresh"));
        let (directory, repository) = repository();
        run_git(directory.path(), &["checkout", "--detach", "HEAD"]);
        let commits = repository.history(100).unwrap();
        assert_eq!(repository.branch(), Some(commits[0].short_id.clone()));
    }
}
