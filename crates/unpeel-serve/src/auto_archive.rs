//! Auto-stop-and-archive: the Host-owned sweep that gives a continuously idle
//! Session the same treatment as the Stop verb once it has sat idle for the
//! workspace's `auto_stop_archive_minutes`. This used to run only inside the
//! Mac app and the interactive terminal UI; a headless `unpeel serve` box
//! now runs it too, against the same setting and the same
//! `session_ops::archive_session` locking path.
//!
//! Rules carried over unchanged: the idle clock is anchored to the canonical
//! hook lifecycle stamp (raw repaints never postpone it), pinned/archived/
//! unread rows are never touched, only one archive is in flight at a time,
//! a failed attempt backs off for a minute, and a stopped Session whose
//! archive marker failed is retried without ever running again.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use crate::sessions::{SessionRow, Status};

pub const SETTING_KEY: &str = "auto_stop_archive_minutes";
/// Same option set the Host's `settings.workspace.set` whitelist accepts.
pub const MINUTE_OPTIONS: [u64; 7] = [0, 30, 60, 120, 240, 480, 1440];
/// The app's `defaultAutoStopArchiveMinutes` — one day.
pub const DEFAULT_MINUTES: u64 = 1440;
const RETRY_DELAY_MS: u64 = 60_000;

/// Effective cutoff from the raw app-state document. Junk never silently
/// *shortens* the cutoff — it reads as off; an absent key is the default.
pub fn minutes_from_state(state: &serde_json::Value) -> u64 {
    match state.get(SETTING_KEY).and_then(|value| value.as_u64()) {
        Some(minutes) if MINUTE_OPTIONS.contains(&minutes) => minutes,
        Some(_) => 0,
        None => DEFAULT_MINUTES,
    }
}

pub fn minutes_from_disk() -> u64 {
    let state: serde_json::Value = std::fs::read(unpeel_core::app_paths::app_state_path())
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default();
    minutes_from_state(&state)
}

#[derive(Debug)]
pub struct Outcome {
    pub session_id: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepEvent {
    Archived(String),
    Failed {
        session_id: String,
        error: String,
    },
    /// A running row the sweep will not archive, with the first blocking
    /// condition; emitted once per row per reason change.
    Skipped {
        session_id: String,
        reason: &'static str,
    },
}

pub struct Sweeper {
    /// When this worker started observing. A Session must be seen idle by
    /// THIS Host for the whole cutoff: a stale lifecycle stamp from before
    /// the worker existed never archives anything on the first sweep, so a
    /// Host restart (or a freshly adopted home) is not a mass archive.
    started_at_ms: u64,
    idle_since_ms: HashMap<String, u64>,
    /// Last reported skip reason per row, so the trace line fires once per
    /// reason change instead of once per tick.
    skip_reasons: HashMap<String, &'static str>,
    issued: HashSet<String>,
    retry_after_ms: HashMap<String, u64>,
    outcomes_tx: mpsc::Sender<Outcome>,
    outcomes_rx: mpsc::Receiver<Outcome>,
}

impl Default for Sweeper {
    fn default() -> Self {
        let (outcomes_tx, outcomes_rx) = mpsc::channel();
        Self {
            started_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or(0),
            idle_since_ms: HashMap::new(),
            skip_reasons: HashMap::new(),
            issued: HashSet::new(),
            retry_after_ms: HashMap::new(),
            outcomes_tx,
            outcomes_rx,
        }
    }
}

impl Sweeper {
    #[cfg(test)]
    fn starting_at(started_at_ms: u64) -> Self {
        Self {
            started_at_ms,
            ..Self::default()
        }
    }

    /// One sweep over the current sidebar rows. `minutes == 0` disables the
    /// cutoff but still drains outcomes and keeps the idle clocks honest.
    pub fn step(
        &mut self,
        rows: &[SessionRow],
        unread: &HashSet<String>,
        minutes: u64,
        now_ms: u64,
    ) -> Vec<SweepEvent> {
        let mut events = Vec::new();
        while let Ok(outcome) = self.outcomes_rx.try_recv() {
            events.push(match &outcome.error {
                Some(error) => SweepEvent::Failed {
                    session_id: outcome.session_id.clone(),
                    error: error.clone(),
                },
                None => SweepEvent::Archived(outcome.session_id.clone()),
            });
            apply_outcome(&mut self.issued, &mut self.retry_after_ms, outcome, now_ms);
        }

        let started_at_ms = self.started_at_ms;
        for row in rows {
            if row.running && row.status == Status::Idle {
                self.idle_since_ms
                    .entry(row.id.clone())
                    .and_modify(|idle_since| {
                        *idle_since =
                            observed_idle_since(Some(*idle_since), row.activity_at, now_ms)
                    })
                    .or_insert_with(|| {
                        observed_idle_since(None, row.activity_at, now_ms).max(started_at_ms)
                    });
            } else {
                self.idle_since_ms.remove(&row.id);
            }
        }
        let live = |id: &String| rows.iter().any(|row| row.id == *id);
        self.idle_since_ms.retain(|id, _| live(id));
        self.skip_reasons.retain(|id, _| live(id));
        self.issued.retain(live);
        self.retry_after_ms.retain(|id, _| live(id));

        if !worker_available(&self.issued) || minutes == 0 {
            return events;
        }
        let (due, skips) = self.next_due(rows, unread, minutes * 60_000, now_ms);
        for (id, reason) in skips {
            if self.skip_reasons.get(&id) != Some(&reason) {
                self.skip_reasons.insert(id.clone(), reason);
                events.push(SweepEvent::Skipped {
                    session_id: id,
                    reason,
                });
            }
        }
        let Some(id) = due else {
            return events;
        };
        self.skip_reasons.remove(&id);
        self.issued.insert(id.clone());
        let outcomes = self.outcomes_tx.clone();
        std::thread::Builder::new()
            .name("unpeel-auto-archive".into())
            .spawn(move || {
                let error = unpeel_core::session_ops::archive_session(&id).err();
                let _ = outcomes.send(Outcome {
                    session_id: id,
                    error,
                });
            })
            .ok();
        events
    }

    /// The first due row, plus the FIRST blocking condition of every running
    /// row that is not due (the reason a Session is never archived is
    /// otherwise invisible — a headless Session nobody has viewed stays
    /// `unread`, for example).
    fn next_due(
        &self,
        rows: &[SessionRow],
        unread: &HashSet<String>,
        threshold_ms: u64,
        now_ms: u64,
    ) -> (Option<String>, Vec<(String, &'static str)>) {
        let mut skips = Vec::new();
        for row in rows {
            if stopped_retry_due(row, &self.issued, &self.retry_after_ms, now_ms) {
                return (Some(row.id.clone()), skips);
            }
            if !row.running {
                continue;
            }
            let reason = if row.status != Status::Idle {
                "not idle"
            } else if !row.archive_available {
                "no resumable conversation (plain shells are never auto-archived)"
            } else if row.pinned {
                "pinned"
            } else if row.archived {
                "already archived"
            } else if row.unread || unread.contains(&row.id) {
                "unread (nobody has viewed it since its last activity)"
            } else if attempt_blocked(&self.issued, &self.retry_after_ms, &row.id, now_ms) {
                "archive attempt pending or in retry backoff"
            } else if let Some(&idle_since) = self.idle_since_ms.get(&row.id) {
                if now_ms.saturating_sub(idle_since) < threshold_ms {
                    "idle for less than the cutoff"
                } else {
                    return (Some(row.id.clone()), skips);
                }
            } else {
                "idle clock not started"
            };
            skips.push((row.id.clone(), reason));
        }
        (None, skips)
    }
}

fn attempt_blocked(
    issued: &HashSet<String>,
    retry_after_ms: &HashMap<String, u64>,
    session_id: &str,
    now_ms: u64,
) -> bool {
    issued.contains(session_id)
        || retry_after_ms
            .get(session_id)
            .is_some_and(|retry_at| now_ms < *retry_at)
}

fn worker_available(issued: &HashSet<String>) -> bool {
    issued.is_empty()
}

fn stopped_retry_due(
    row: &SessionRow,
    issued: &HashSet<String>,
    retry_after_ms: &HashMap<String, u64>,
    now_ms: u64,
) -> bool {
    !row.running
        && row.archive_available
        && !row.archived
        && !row.pinned
        && !issued.contains(&row.id)
        && retry_after_ms
            .get(&row.id)
            .is_some_and(|retry_at| now_ms >= *retry_at)
}

fn apply_outcome(
    issued: &mut HashSet<String>,
    retry_after_ms: &mut HashMap<String, u64>,
    outcome: Outcome,
    now_ms: u64,
) {
    let Outcome { session_id, error } = outcome;
    issued.remove(&session_id);
    match error {
        Some(_) => {
            retry_after_ms.insert(session_id, now_ms.saturating_add(RETRY_DELAY_MS));
        }
        None => {
            retry_after_ms.remove(&session_id);
        }
    }
}

fn observed_idle_since(existing: Option<u64>, lifecycle_at: u64, now_ms: u64) -> u64 {
    let observed = lifecycle_at.min(now_ms);
    match existing {
        Some(anchor) => anchor.max(observed),
        None if lifecycle_at == 0 => now_ms,
        None => observed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_clock_advances_only_with_the_canonical_lifecycle_stamp() {
        let first = observed_idle_since(None, 1_000, 2_000);
        assert_eq!(first, 1_000);
        // Raw repaints see the same lifecycle stamp: the clock stays anchored.
        assert_eq!(observed_idle_since(Some(first), 1_000, 90_000_000), 1_000);
        // A real lifecycle event moves it forward.
        assert_eq!(
            observed_idle_since(Some(first), 80_000_000, 90_000_000),
            80_000_000
        );
        // Future-skewed timestamps cannot postpone cleanup past now.
        assert_eq!(
            observed_idle_since(None, 100_000_000, 90_000_000),
            90_000_000
        );
        // A missing stamp starts a fresh clock instead of the epoch.
        assert_eq!(observed_idle_since(None, 0, 90_000_000), 90_000_000);
    }

    #[test]
    fn failed_attempt_releases_in_flight_and_retries_after_backoff() {
        let mut issued = HashSet::from(["s1".to_string()]);
        let mut retry_after = HashMap::new();
        apply_outcome(
            &mut issued,
            &mut retry_after,
            Outcome {
                session_id: "s1".into(),
                error: Some("temporary host failure".into()),
            },
            1_000,
        );
        assert!(!issued.contains("s1"));
        assert!(attempt_blocked(
            &issued,
            &retry_after,
            "s1",
            1_000 + RETRY_DELAY_MS - 1
        ));
        assert!(!attempt_blocked(
            &issued,
            &retry_after,
            "s1",
            1_000 + RETRY_DELAY_MS
        ));
        issued.insert("s1".into());
        assert!(attempt_blocked(&issued, &retry_after, "s1", u64::MAX));
        apply_outcome(
            &mut issued,
            &mut retry_after,
            Outcome {
                session_id: "s1".into(),
                error: None,
            },
            u64::MAX,
        );
        assert!(issued.is_empty() && retry_after.is_empty());
    }

    #[test]
    fn a_stale_lifecycle_stamp_never_archives_on_the_first_sweep() {
        // A fixture-like row: idle for a year according to its stamp, but
        // this worker has only just started observing it.
        let row = SessionRow {
            id: "old".into(),
            project_id: "p".into(),
            label: "old".into(),
            command: "claude".into(),
            active_runtime_id: None,
            active_app: None,
            resume_available: false,
            archive_available: true,
            resume_agent_available: false,
            running: true,
            status: Status::Idle,
            created_at: 1_000,
            pinned: false,
            archived: false,
            unread: false,
            latest_alert_body: None,
            cwd: "/tmp".into(),
            activity_at: 1_000,
            group_id: "p".into(),
            detected_local_urls: Vec::new(),
        };
        let now = 90_000_000_000;
        let mut sweeper = Sweeper::starting_at(now);
        sweeper.step(std::slice::from_ref(&row), &HashSet::new(), 60, now);
        assert_eq!(sweeper.idle_since_ms.get("old"), Some(&now));
        assert!(sweeper
            .next_due(
                std::slice::from_ref(&row),
                &HashSet::new(),
                60 * 60_000,
                now
            )
            .0
            .is_none());
        // Once the worker has watched it idle for the cutoff, it is due.
        let later = now + 60 * 60_000;
        assert_eq!(
            sweeper
                .next_due(
                    std::slice::from_ref(&row),
                    &HashSet::new(),
                    60 * 60_000,
                    later
                )
                .0,
            Some("old".to_string())
        );
    }

    #[test]
    fn only_one_archive_is_in_flight_at_a_time() {
        assert!(worker_available(&HashSet::new()));
        assert!(!worker_available(&HashSet::from(["s1".to_string()])));
    }

    #[test]
    fn junk_or_absent_setting_reads_as_off_or_default() {
        assert_eq!(minutes_from_state(&serde_json::json!({})), DEFAULT_MINUTES);
        assert_eq!(
            minutes_from_state(&serde_json::json!({ SETTING_KEY: 60 })),
            60
        );
        assert_eq!(
            minutes_from_state(&serde_json::json!({ SETTING_KEY: 7 })),
            0
        );
        // A non-numeric value is not a number at all: it reads as absent.
        assert_eq!(
            minutes_from_state(&serde_json::json!({ SETTING_KEY: "1440" })),
            DEFAULT_MINUTES
        );
    }

    fn idle_row(id: &str, activity_at: u64) -> SessionRow {
        SessionRow {
            id: id.into(),
            project_id: "p".into(),
            label: id.into(),
            command: "claude".into(),
            active_runtime_id: None,
            active_app: None,
            resume_available: false,
            archive_available: true,
            resume_agent_available: false,
            running: true,
            status: Status::Idle,
            created_at: 1_000,
            pinned: false,
            archived: false,
            unread: false,
            latest_alert_body: None,
            cwd: "/tmp".into(),
            activity_at,
            group_id: "p".into(),
            detected_local_urls: Vec::new(),
        }
    }

    /// The first blocking condition is reported once per row per reason
    /// change — an unviewed (unread) Session is the headless case that
    /// otherwise never archives and never says why.
    #[test]
    fn skipped_rows_report_their_first_blocking_reason_once() {
        let now = 90_000_000_000;
        let mut row = idle_row("quiet", now - 3 * 60 * 60_000);
        row.unread = true;
        let mut sweeper = Sweeper::starting_at(now - 3 * 60 * 60_000);
        let first = sweeper.step(std::slice::from_ref(&row), &HashSet::new(), 60, now);
        assert!(
            matches!(
                first.as_slice(),
                [SweepEvent::Skipped { session_id, reason }]
                    if session_id == "quiet" && reason.starts_with("unread")
            ),
            "{first:?}"
        );
        let again = sweeper.step(std::slice::from_ref(&row), &HashSet::new(), 60, now + 1_000);
        assert!(again.is_empty(), "same reason must not repeat: {again:?}");
        row.unread = false;
        row.pinned = true;
        let changed = sweeper.step(std::slice::from_ref(&row), &HashSet::new(), 60, now + 2_000);
        assert!(
            matches!(
                changed.as_slice(),
                [SweepEvent::Skipped {
                    reason: "pinned",
                    ..
                }]
            ),
            "{changed:?}"
        );
    }
}
