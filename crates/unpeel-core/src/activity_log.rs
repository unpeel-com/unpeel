//! Persisted cross-frontend session activity history.
//!
//! The native app and the TUI share an append-only JSONL feed at
//! `<UNPEEL_HOME>/activity-log.jsonl`. Entry metadata is snapshotted when an
//! event happens so the feed remains renderable after its session or project
//! is removed.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum number of collapsed entries retained in memory and after a
/// compaction.
pub const MAX_ENTRIES: usize = 300;

const COMPACT_AT_PHYSICAL_LINES: usize = MAX_ENTRIES * 2;
const FILE_NAME: &str = "activity-log.jsonl";

/// A session activity transition recorded in the shared feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityLogKind {
    Started,
    NeedsInput,
    Finished,
    Exited,
    /// An App-owned informational alert. It never changes Session lifecycle.
    Alert,
}

/// One line in `activity-log.jsonl`.
///
/// Field names intentionally match `ActivityLogEntry.CodingKeys` in the
/// native app's `ActivityLog.swift`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityLogEntry {
    pub id: String,
    pub session_id: String,
    pub kind: ActivityLogKind,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub title: String,
    /// Launch command, used to choose the CLI/provider icon.
    pub command: String,
    pub project_id: String,
    pub project_name: String,
    /// App-provided alert copy. Absent for lifecycle entries and legacy logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// In-memory view of the shared append-only activity feed.
#[derive(Debug)]
pub struct ActivityLogStore {
    path: PathBuf,
    entries: Vec<ActivityLogEntry>,
    physical_line_count: usize,
}

impl Default for ActivityLogStore {
    /// An empty store at the default path. This deliberately performs no I/O,
    /// making `ActivityLogStore::load_default().unwrap_or_default()` a safe
    /// infallible frontend initialization path.
    fn default() -> Self {
        Self::empty_at(default_path())
    }
}

impl ActivityLogStore {
    /// Load the feed for the current `UNPEEL_HOME`.
    pub fn load_default() -> io::Result<Self> {
        Self::load_from(default_path())
    }

    /// Load a feed from an explicit path.
    ///
    /// This is public so workspace-aware callers and tests do not need to
    /// mutate the process-global `UNPEEL_HOME` environment variable.
    pub fn load_from(path: impl Into<PathBuf>) -> io::Result<Self> {
        let mut store = Self::empty_at(path.into());
        store.refresh()?;
        Ok(store)
    }

    /// Entries in oldest-to-newest feed order.
    pub fn entries(&self) -> &[ActivityLogEntry] {
        &self.entries
    }

    /// Re-read the file, skipping malformed lines and applying the same
    /// repeat-collapse rule as [`Self::append`]. A missing file is an empty
    /// feed, not an error.
    pub fn refresh(&mut self) -> io::Result<()> {
        let raw = match fs::read(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.entries.clear();
                self.physical_line_count = 0;
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        let physical_line_count = count_physical_lines(&raw);
        let mut entries = Vec::new();
        for line in raw.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_slice::<ActivityLogEntry>(line) else {
                continue;
            };
            append_collapsing(&mut entries, entry);
            trim_to_limit(&mut entries);
        }

        self.entries = entries;
        self.physical_line_count = physical_line_count;
        if self.physical_line_count >= COMPACT_AT_PHYSICAL_LINES {
            self.compact()?;
        }
        Ok(())
    }

    /// Append an event to the JSONL feed and update the in-memory view.
    ///
    /// Same-kind repeats for the same session replace that session's latest
    /// entry, bumping it to the end of the feed. The complete encoded line is
    /// passed to an `O_APPEND` file in one `write_all` operation so concurrent
    /// appenders cannot share a seek offset.
    pub fn append(&mut self, entry: ActivityLogEntry) -> io::Result<()> {
        let mut line = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        line.push(b'\n');

        // Match the native store: the current frontend still shows a newly
        // observed event if persistence happens to fail.
        append_collapsing(&mut self.entries, entry);
        trim_to_limit(&mut self.entries);

        ensure_parent(&self.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&line)?;
        file.flush()?;
        self.physical_line_count = self.physical_line_count.saturating_add(1);

        if self.physical_line_count >= COMPACT_AT_PHYSICAL_LINES {
            self.compact()?;
        }
        Ok(())
    }

    fn empty_at(path: PathBuf) -> Self {
        Self {
            path,
            entries: Vec::new(),
            physical_line_count: 0,
        }
    }

    /// Atomically rewrite the file to the collapsed in-memory tail.
    fn compact(&mut self) -> io::Result<()> {
        ensure_parent(&self.path)?;

        let mut body = Vec::new();
        for entry in &self.entries {
            serde_json::to_writer(&mut body, entry).map_err(io::Error::other)?;
            body.push(b'\n');
        }

        let (temporary_path, mut temporary_file) = create_compaction_file(&self.path)?;
        let result = (|| {
            temporary_file.write_all(&body)?;
            temporary_file.flush()?;
            temporary_file.sync_all()?;
            drop(temporary_file);
            fs::rename(&temporary_path, &self.path)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        } else {
            self.physical_line_count = self.entries.len();
        }
        result
    }
}

fn default_path() -> PathBuf {
    crate::app_paths::unpeel_home().join(FILE_NAME)
}

fn append_collapsing(entries: &mut Vec<ActivityLogEntry>, entry: ActivityLogEntry) {
    if let Some(index) = entries
        .iter()
        .rposition(|existing| existing.session_id == entry.session_id)
    {
        if entries[index].kind == entry.kind
            && (entry.kind != ActivityLogKind::Alert || entries[index].message == entry.message)
        {
            entries.remove(index);
        }
    }
    entries.push(entry);
}

fn trim_to_limit(entries: &mut Vec<ActivityLogEntry>) {
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
}

fn count_physical_lines(raw: &[u8]) -> usize {
    let newline_count = raw.iter().filter(|byte| **byte == b'\n').count();
    newline_count + usize::from(!raw.is_empty() && raw.last() != Some(&b'\n'))
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn create_compaction_file(path: &Path) -> io::Result<(PathBuf, File)> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // `create_new` makes even the vanishingly unlikely name collision safe.
    // Retry with a new counter rather than truncating another writer's temp.
    for _ in 0..16 {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = OsString::from(file_name);
        temporary_name.push(format!(
            ".unpeel-tmp.{}.{}.{}",
            std::process::id(),
            now,
            sequence
        ));
        let temporary_path = parent.join(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an activity-log compaction file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn explicit_test_path(label: &str) -> (PathBuf, PathBuf) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "unpeel-activity-log-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(FILE_NAME);
        (directory, path)
    }

    fn entry(id: usize, session_id: &str, kind: ActivityLogKind) -> ActivityLogEntry {
        ActivityLogEntry {
            id: format!("event-{id}"),
            session_id: session_id.to_string(),
            kind,
            at: 1_700_000_000_000 + id as u64,
            title: format!("Session {id}"),
            command: "codex --continue".to_string(),
            project_id: "project-1".to_string(),
            project_name: "Unpeel".to_string(),
            message: None,
        }
    }

    fn encode_lines(entries: impl IntoIterator<Item = ActivityLogEntry>) -> Vec<u8> {
        let mut raw = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut raw, &entry).unwrap();
            raw.push(b'\n');
        }
        raw
    }

    #[test]
    fn schema_matches_the_native_jsonl_contract() {
        let entry = entry(7, "session-7", ActivityLogKind::NeedsInput);
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "id": "event-7",
                "session_id": "session-7",
                "kind": "needs_input",
                "at": 1_700_000_000_007_u64,
                "title": "Session 7",
                "command": "codex --continue",
                "project_id": "project-1",
                "project_name": "Unpeel"
            })
        );
        assert_eq!(
            serde_json::from_value::<ActivityLogEntry>(value).unwrap(),
            entry
        );
    }

    #[test]
    fn alert_schema_is_additive_and_only_identical_repeats_collapse() {
        let mut first = entry(1, "session-a", ActivityLogKind::Alert);
        first.message = Some("Close to the weekly limit".to_string());
        let mut duplicate = entry(2, "session-a", ActivityLogKind::Alert);
        duplicate.message = first.message.clone();
        let mut distinct = entry(3, "session-a", ActivityLogKind::Alert);
        distinct.message = Some("Weekly limit reached".to_string());

        let value = serde_json::to_value(&first).unwrap();
        assert_eq!(value["kind"], "alert");
        assert_eq!(value["message"], "Close to the weekly limit");

        let mut entries = Vec::new();
        append_collapsing(&mut entries, first);
        append_collapsing(&mut entries, duplicate);
        append_collapsing(&mut entries, distinct);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "event-2");
        assert_eq!(entries[1].id, "event-3");
    }

    #[test]
    fn load_skips_malformed_lines_and_collapses_only_the_latest_session_kind() {
        let (directory, path) = explicit_test_path("load");
        let mut raw = encode_lines([
            entry(1, "session-a", ActivityLogKind::Started),
            entry(2, "session-b", ActivityLogKind::Finished),
            entry(3, "session-a", ActivityLogKind::Started),
            entry(4, "session-a", ActivityLogKind::Finished),
            entry(5, "session-a", ActivityLogKind::Started),
        ]);
        raw.extend_from_slice(b"not json\n");
        fs::write(&path, raw).unwrap();

        let store = ActivityLogStore::load_from(&path).unwrap();
        let ids: Vec<&str> = store
            .entries()
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        // event-1 is replaced by event-3. Event-3 remains because event-4
        // changed session-a's latest kind before event-5 arrived.
        assert_eq!(ids, ["event-2", "event-3", "event-4", "event-5"]);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn append_persists_updates_memory_and_refreshes_external_changes() {
        let (directory, path) = explicit_test_path("append-refresh");
        let mut store = ActivityLogStore::load_from(&path).unwrap();
        store
            .append(entry(1, "session-a", ActivityLogKind::Finished))
            .unwrap();
        assert_eq!(store.entries()[0].id, "event-1");
        assert_eq!(count_physical_lines(&fs::read(&path).unwrap()), 1);

        let mut external = OpenOptions::new().append(true).open(&path).unwrap();
        external.write_all(b"malformed\n").unwrap();
        external
            .write_all(&encode_lines([entry(
                2,
                "session-a",
                ActivityLogKind::Finished,
            )]))
            .unwrap();
        drop(external);

        store.refresh().unwrap();
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].id, "event-2");
        assert_eq!(store.physical_line_count, 3);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn load_keeps_only_the_newest_three_hundred_collapsed_entries() {
        let (directory, path) = explicit_test_path("limit");
        fs::write(
            &path,
            encode_lines(
                (0..350).map(|id| entry(id, &format!("session-{id}"), ActivityLogKind::Started)),
            ),
        )
        .unwrap();

        let store = ActivityLogStore::load_from(&path).unwrap();
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert_eq!(store.entries().first().unwrap().id, "event-50");
        assert_eq!(store.entries().last().unwrap().id, "event-349");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn six_hundred_physical_lines_compact_atomically_to_the_memory_tail() {
        let (directory, path) = explicit_test_path("compact");
        let mut raw = b"malformed\n".repeat(COMPACT_AT_PHYSICAL_LINES - 1);
        raw.extend_from_slice(&encode_lines([entry(
            1,
            "session-a",
            ActivityLogKind::Exited,
        )]));
        fs::write(&path, raw).unwrap();

        let store = ActivityLogStore::load_from(&path).unwrap();
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.physical_line_count, 1);
        let compacted = fs::read(&path).unwrap();
        assert_eq!(count_physical_lines(&compacted), 1);
        assert_eq!(
            serde_json::from_slice::<ActivityLogEntry>(compacted.strip_suffix(b"\n").unwrap())
                .unwrap()
                .id,
            "event-1"
        );
        assert!(fs::read_dir(&directory).unwrap().all(|item| !item
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("tmp")));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_six_hundredth_append_triggers_compaction() {
        let (directory, path) = explicit_test_path("append-compact");
        fs::write(
            &path,
            encode_lines(
                (0..COMPACT_AT_PHYSICAL_LINES - 1)
                    .map(|id| entry(id, &format!("session-{id}"), ActivityLogKind::Started)),
            ),
        )
        .unwrap();

        let mut store = ActivityLogStore::load_from(&path).unwrap();
        assert_eq!(store.physical_line_count, COMPACT_AT_PHYSICAL_LINES - 1);
        store
            .append(entry(
                COMPACT_AT_PHYSICAL_LINES,
                "last-session",
                ActivityLogKind::Finished,
            ))
            .unwrap();
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert_eq!(store.physical_line_count, MAX_ENTRIES);
        assert_eq!(count_physical_lines(&fs::read(&path).unwrap()), MAX_ENTRIES);

        fs::remove_dir_all(directory).unwrap();
    }
}
