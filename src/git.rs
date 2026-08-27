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
}
