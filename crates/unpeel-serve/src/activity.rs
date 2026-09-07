//! Port of the native busy/idle/attention engine (`SessionActivity.swift`).
//! Hook-capable tools are hook-owned: the first hook event latches the
//! session, and from then on only hook events and the 5-minute
//! output-rearmed timeout change its state — raw output growth never flips
//! busy/idle directly. Hookless agents and ordinary terminal programs remain
//! visually neutral because output/screen changes have no lifecycle authority.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const HOOK_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Compatibility window for hooks installed by an older Unpeel build, before
/// lifecycle payloads carried `unpeel_runtime_generation`. Immediately after
/// an in-place generation edge, an untagged Stop is ambiguous: it may be a
/// background reporter from the process we just terminated. Suppress it until
/// the replacement proves its own lifecycle with Start/UserPromptSubmit. The
/// time bound prevents a permanently old hook install from losing Stops
/// forever if that provider emits no recognizable opening event.
const LEGACY_GENERATION_STOP_GUARD: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookState {
    Busy,
    Idle,
    Attention,
}

#[derive(Default)]
struct Entry {
    /// Foreground lifecycle and background work are independent. A main Stop
    /// (or ESC) must not complete a child that is still reporting work.
    background: HashMap<String, BackgroundActivity>,
    background_expired: bool,
    /// A provider-reported cancellation needs only its next opening hook to
    /// rearm; unlike inferred input cancellation, it also covers client stops.
    native_cancelled: bool,
    last_cancelled_at: Option<u64>,
    cancelled_at: Option<u64>,
    submitted_at: Option<u64>,
    pending_opener: Option<(String, Option<String>, SystemTime)>,
    last_turn_started_at: Option<u64>,
    completed: bool,
    /// Host-owned generation of the managed runtime inside this PTY. An
    /// in-place agent restart increments it while the Session id remains the
    /// same, so the old runtime's hook latch must not bleed into the new one.
    runtime_launch_generation: u64,
    hook_seen: bool,
    state: Option<HookState>,
    deadline_at: Option<SystemTime>,
    /// Last observed activity signal (the host's screen-change stamp, or
    /// output.bin's size under old hosts); "grew" = the value changed.
    last_signal: Option<u64>,
    /// Newest accepted live/durable activity transition. Metadata-only hooks
    /// do not supersede a missed durable transition.
    last_transition_at: Option<SystemTime>,
    /// Set by the managed-session scan; used to persist timeout decisions.
    session_dir: Option<PathBuf>,
    /// Exact provenance for current hook assets. When present, this wins over
    /// wall-clock ordering across a manifest-commit race.
    last_hook_generation: Option<u64>,
    /// Generation for which a Start/UserPromptSubmit has been accepted. This
    /// makes the bounded legacy-hook fallback safe: untagged Stops after a
    /// restart remain quarantined until the replacement runtime starts a turn.
    confirmed_turn_generation: Option<u64>,
    /// Opening-event evidence from an older hook asset with no generation
    /// field. If it beats the manifest commit into the TUI but arrived after
    /// the replacement launch timestamp, the generation edge may rebind this
    /// opener. A lone untagged Stop never sets this field.
    legacy_turn_started_at: Option<SystemTime>,
    /// Last observed foreground-runtime identity (`runtime_id:pid`), tracked
    /// only for sessions whose launch command is not hook-capable (blank
    /// terminals, custom commands). Hook events carry no runtime identity, so
    /// this is what ties a latch to the process that produced it.
    foreground_identity: Option<String>,
    /// Distinguishes "never observed by this engine" from "observed with no
    /// foreground runtime". An engine that starts mid-run must not treat its
    /// first sighting as an edge and drop a latch built from live events it
    /// already accepted.
    foreground_identity_recorded: bool,
}

struct BackgroundActivity {
    started_at: SystemTime,
    deadline_at: SystemTime,
    expired: bool,
}

/// Canonical event name from the wire's case/dash/space/underscore-insensitive
/// variants (mirrors `HookServer.normalizedHookEventName`).
pub fn normalize_event_name(raw: &str) -> String {
    let key: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect();
    match key.as_str() {
        "start" => "Start".into(),
        // Session open / in-tool resume, not a turn. Same rationale as
        // HookServer.normalizedHookEventName: Grok posts session_start, and
        // treating that as Start spins the session from launch.
        "sessionstart" => "HookSeen".into(),
        "userpromptsubmit" | "userpromptsubmitted" | "beforesubmitprompt" => {
            "UserPromptSubmit".into()
        }
        "stop" | "sessionend" => "Stop".into(),
        // Child activity is reconciled from independent durable markers;
        // neither edge replaces the main turn's lifecycle.
        "subagentstart" | "subagentstop" => "HookSeen".into(),
        "stopfailure" => "StopFailure".into(),
        "stopcancelled" | "stopcanceled" => "StopCancelled".into(),
        "idle" => "Idle".into(),
        "permissionrequest" => "PermissionRequest".into(),
        _ => raw.trim().to_string(),
    }
}

fn starts_turn(canonical: &str) -> bool {
    matches!(canonical, "Start" | "UserPromptSubmit")
}

/// AskUserQuestion permission prompts latch hook ownership but change no
/// state (the question renders in-terminal; attention would double-signal).
fn is_latch_only(canonical: &str, tool_name: Option<&str>) -> bool {
    match canonical {
        "Start" | "UserPromptSubmit" | "Stop" | "StopFailure" | "StopCancelled" | "Idle" => false,
        "PermissionRequest" => tool_name == Some("AskUserQuestion"),
        _ => true,
    }
}

#[derive(Default)]
pub struct ActivityEngine {
    entries: HashMap<String, Entry>,
}

impl ActivityEngine {
    pub fn apply_hook_event(
        &mut self,
        session_id: &str,
        raw_name: &str,
        tool_name: Option<&str>,
        now: SystemTime,
    ) {
        self.apply_hook_event_unchecked(session_id, raw_name, tool_name, now);
    }

    fn apply_hook_event_unchecked(
        &mut self,
        session_id: &str,
        raw_name: &str,
        tool_name: Option<&str>,
        now: SystemTime,
    ) -> bool {
        let canonical = normalize_event_name(raw_name);
        let latch_only = is_latch_only(&canonical, tool_name);
        let entry = self.entries.entry(session_id.to_string()).or_default();
        if entry.native_cancelled {
            if starts_turn(&canonical) {
                entry.native_cancelled = false;
            } else {
                return false;
            }
        }
        if let Some(cancelled_at) = entry.cancelled_at {
            let event_at = unix_ms(now);
            if starts_turn(&canonical)
                && entry
                    .submitted_at
                    .is_some_and(|submitted| submitted > cancelled_at && event_at >= submitted)
            {
                entry.cancelled_at = None;
                entry.pending_opener = None;
            } else {
                // Input is persisted by the PTY's timer. A fast provider can
                // post its opening hook first; retain it until the matching
                // submission marker arrives instead of losing the new turn.
                if starts_turn(&canonical) && event_at >= cancelled_at {
                    entry.pending_opener = Some((canonical, tool_name.map(str::to_owned), now));
                }
                return false;
            }
        }
        entry.hook_seen = true;
        if latch_only {
            return true;
        }
        // A provider idle heartbeat repairs a missing stop but cannot change
        // an already-known successful, failed, or cancelled turn's outcome.
        if canonical == "Idle" && entry.state == Some(HookState::Idle) {
            return true;
        }
        entry.last_transition_at = Some(now);
        match canonical.as_str() {
            "Start" | "UserPromptSubmit" => {
                entry.completed = false;
                entry.background_expired = false;
                entry.last_turn_started_at = Some(unix_ms(now));
                entry.state = Some(HookState::Busy);
                entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
            }
            "Stop" | "StopFailure" => {
                entry.completed = canonical == "Stop";
                entry.state = Some(HookState::Idle);
                entry.deadline_at = None;
            }
            "StopCancelled" => {
                entry.native_cancelled = true;
                entry.completed = false;
                entry.state = Some(HookState::Idle);
                entry.deadline_at = None;
            }
            "Idle" => {
                entry.completed = false;
                entry.state = Some(HookState::Idle);
                entry.deadline_at = None;
            }
            "PermissionRequest" => {
                entry.completed = false;
                entry.state = Some(HookState::Attention);
                entry.deadline_at = None;
                // Re-baseline output tracking so the answer's redraw counts
                // as fresh growth.
                entry.last_signal = None;
            }
            _ => {}
        }
        true
    }

    /// Apply one hook only when its runtime provenance can belong to the
    /// current managed launch. Current hook assets carry an exact generation;
    /// old assets use the bounded Start-before-Stop fallback above.
    ///
    /// Returns false when the event was stale/ambiguous and changed no latch or
    /// activity state. Callers use that to suppress completion history and
    /// unread side effects as well as the visible status transition.
    // The full launch proof is intentionally passed together so callers
    // cannot apply a hook without its runtime generation and launch epoch.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_hook_event_for_runtime(
        &mut self,
        session_id: &str,
        raw_name: &str,
        tool_name: Option<&str>,
        now: SystemTime,
        event_generation: Option<u64>,
        current_generation: u64,
        launched_at_unix_ms: Option<u64>,
    ) -> bool {
        self.observe_runtime_launch(session_id, current_generation, launched_at_unix_ms);
        if event_generation.is_some_and(|generation| generation < current_generation) {
            return false;
        }

        let canonical = normalize_event_name(raw_name);
        let launched_at = launched_at_unix_ms
            .map(|milliseconds| SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds));
        if event_generation.is_none() && launched_at.is_some_and(|launch| now < launch) {
            return false;
        }

        let entry = self.entries.entry(session_id.to_string()).or_default();
        if starts_turn(&canonical) {
            // Explicit future-generation events can beat the Host's manifest
            // commit into this process. Remember their own generation so the
            // subsequent manifest edge recognizes that the new runtime has
            // already established its lifecycle.
            entry.confirmed_turn_generation = Some(event_generation.unwrap_or(current_generation));
            if event_generation.is_none() {
                entry.legacy_turn_started_at = Some(now);
            }
        } else if matches!(
            canonical.as_str(),
            "Stop" | "StopFailure" | "StopCancelled" | "Idle"
        ) && event_generation.is_none()
            && current_generation > 1
            && entry.confirmed_turn_generation != Some(current_generation)
            && launched_at.is_some_and(|launch| {
                now.duration_since(launch).unwrap_or_default() < LEGACY_GENERATION_STOP_GUARD
            })
        {
            return false;
        }

        if !self.apply_hook_event_unchecked(session_id, &canonical, tool_name, now) {
            return false;
        }
        if let Some(entry) = self.entries.get_mut(session_id) {
            entry.last_hook_generation = event_generation;
        }
        true
    }

    pub fn is_latched(&self, session_id: &str) -> bool {
        self.entries
            .get(session_id)
            .map(|e| e.hook_seen)
            .unwrap_or(false)
    }

    pub fn hook_owned_state(&self, session_id: &str) -> Option<HookState> {
        let entry = self.entries.get(session_id)?;
        if !entry.hook_seen {
            return None;
        }
        if entry.state == Some(HookState::Attention) {
            return entry.state;
        }
        if entry.background.values().any(|activity| !activity.expired) {
            return Some(HookState::Busy);
        }
        entry.state
    }

    pub fn is_cancelled(&self, session_id: &str) -> bool {
        self.entries
            .get(session_id)
            .is_some_and(|entry| entry.cancelled_at.is_some() || entry.native_cancelled)
    }

    pub fn is_completed(&self, session_id: &str) -> bool {
        self.entries.get(session_id).is_some_and(|entry| {
            entry.hook_seen
                && entry.state == Some(HookState::Idle)
                && entry.completed
                && !entry.background_expired
                && entry.background.values().all(|activity| activity.expired)
        })
    }

    fn sync_background_from_disk(
        &mut self,
        session_id: &str,
        dir: &Path,
        generation: u64,
        not_before_unix_ms: Option<u64>,
    ) {
        let Ok(activities) =
            unpeel_core::hook_assets::read_background_hook_activity(dir, generation)
        else {
            return;
        };
        let entry = self.entries.entry(session_id.to_owned()).or_default();
        let mut current = std::collections::HashSet::new();
        for activity in activities {
            if not_before_unix_ms.is_some_and(|epoch| unix_ms(activity.started_at) < epoch) {
                continue;
            }
            current.insert(activity.id.clone());
            if entry
                .background
                .get(&activity.id)
                .is_some_and(|known| known.started_at == activity.started_at)
            {
                continue;
            }
            entry.hook_seen = true;
            entry.background.insert(
                activity.id,
                BackgroundActivity {
                    started_at: activity.started_at,
                    deadline_at: activity.started_at + HOOK_IDLE_TIMEOUT,
                    expired: false,
                },
            );
        }
        entry.background.retain(|id, _| current.contains(id));
    }

    pub fn sync_cancellation_from_disk(&mut self, session_id: &str, dir: &Path, generation: u64) {
        if let Some(marker) = unpeel_core::hook_cancellation::read_in(dir) {
            self.observe_cancellation(session_id, &marker, generation);
        }
    }

    fn observe_cancellation(
        &mut self,
        session_id: &str,
        marker: &unpeel_core::hook_cancellation::Cancellation,
        generation: u64,
    ) {
        if marker.runtime_generation != generation {
            return;
        }
        let entry = self.entries.entry(session_id.to_string()).or_default();
        if entry
            .last_cancelled_at
            .is_none_or(|at| marker.cancelled_at > at)
        {
            entry.last_cancelled_at = Some(marker.cancelled_at);
            entry.pending_opener = None;
            let already_started_next_turn = marker.submitted_at.is_some_and(|submitted| {
                submitted > marker.cancelled_at
                    && entry
                        .last_turn_started_at
                        .is_some_and(|started| started >= submitted)
            });
            if !already_started_next_turn {
                entry.completed = false;
                entry.cancelled_at = Some(marker.cancelled_at);
                if entry.hook_seen {
                    entry.state = Some(HookState::Idle);
                }
                entry.deadline_at = None;
            }
        }
        entry.submitted_at = marker.submitted_at;
        let pending = entry.pending_opener.take();
        if let Some((name, tool, at)) = pending {
            self.apply_hook_event_unchecked(session_id, &name, tool.as_deref(), at);
        }
    }

    pub fn runtime_launch_generation(&self, session_id: &str) -> Option<u64> {
        self.entries
            .get(session_id)
            .map(|entry| entry.runtime_launch_generation)
    }

    /// Observe the Host's generation before deriving activity for this tick.
    /// A new in-place runtime launch resets hook authority unless a hook from
    /// that new launch already arrived ahead of the manifest rescan.
    pub fn observe_runtime_launch(
        &mut self,
        session_id: &str,
        generation: u64,
        launched_at_unix_ms: Option<u64>,
    ) {
        let entry = self.entries.entry(session_id.to_string()).or_default();
        if entry.runtime_launch_generation == generation {
            return;
        }
        let launched_at = launched_at_unix_ms
            .map(|milliseconds| SystemTime::UNIX_EPOCH + Duration::from_millis(milliseconds));
        let saw_exact_generation = entry.last_hook_generation == Some(generation);
        let saw_new_legacy_turn = entry.last_hook_generation.is_none()
            && launched_at.is_some_and(|launched_at| {
                entry
                    .legacy_turn_started_at
                    .is_some_and(|started_at| started_at >= launched_at)
            });
        let already_saw_new_hook = saw_exact_generation || saw_new_legacy_turn;
        if !already_saw_new_hook {
            *entry = Entry::default();
        }
        entry.runtime_launch_generation = generation;
        if saw_new_legacy_turn {
            entry.confirmed_turn_generation = Some(generation);
        }
    }

    /// Foreground-observation edge for sessions whose launch command is not
    /// hook-capable. A hook-capable runtime the user starts by hand inside a
    /// reusable shell is hook-owned once its live events latch — but those
    /// events carry no runtime identity, so a latch left behind by a previous
    /// foreground agent must not claim authority over a newly observed
    /// process (a stale busy/attention latch from a killed run would misstate
    /// its replacement). A change to a new observed identity resets the entry
    /// — hook latch and output baseline both — while keeping the runtime
    /// generation. The first sighting is only recorded, never an edge.
    pub fn observe_foreground_runtime(&mut self, session_id: &str, observed: Option<&str>) {
        let entry = self.entries.entry(session_id.to_string()).or_default();
        let changed_to_new_agent = entry.foreground_identity_recorded
            && observed.is_some()
            && entry.foreground_identity.as_deref() != observed;
        if changed_to_new_agent {
            *entry = Entry {
                runtime_launch_generation: entry.runtime_launch_generation,
                ..Entry::default()
            };
        }
        entry.foreground_identity = observed.map(str::to_string);
        entry.foreground_identity_recorded = true;
    }

    /// Per-tick output observation + timeout sweep for hook-owned sessions.
    /// `allow_attention_clear` is false for tools that repaint their ask-user
    /// UI to the terminal (grok), where growth doesn't mean "user answered".
    /// A settled turn remains idle until another opening hook, regardless of
    /// trailing output, scrolling, resizing, or a Controller reconnect.
    pub fn note_output_and_sweep(
        &mut self,
        session_id: &str,
        activity_signal: u64,
        allow_attention_clear: bool,
        now: SystemTime,
    ) {
        let entry = self.entries.entry(session_id.to_string()).or_default();
        let grew = entry
            .last_signal
            .is_some_and(|previous| previous != activity_signal);
        entry.last_signal = Some(activity_signal);
        // Output can maintain a hook-established background lease, but can
        // never create or revive one. Missing child-stop hooks therefore
        // expire through the same bounded fallback as foreground activity.
        for activity in entry
            .background
            .values_mut()
            .filter(|activity| !activity.expired)
        {
            if grew {
                activity.deadline_at = now + HOOK_IDLE_TIMEOUT;
            } else if activity.deadline_at <= now {
                activity.expired = true;
                entry.background_expired = true;
            }
        }
        if entry.cancelled_at.is_some() || entry.native_cancelled {
            return;
        }
        match entry.state {
            Some(HookState::Attention) if allow_attention_clear && grew => {
                entry.state = Some(HookState::Busy);
                entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
            }
            Some(HookState::Busy) => {
                if entry.deadline_at.is_some_and(|d| d <= now) {
                    entry.completed = false;
                    entry.state = Some(HookState::Idle);
                    entry.deadline_at = None;
                    if let (Some(dir), Some(through)) =
                        (&entry.session_dir, entry.last_transition_at)
                    {
                        if let Err(error) = unpeel_core::hook_assets::record_hook_expiry(
                            dir,
                            entry.runtime_launch_generation,
                            through,
                        ) {
                            unpeel_core::hook_assets::append_trace_log_line(&format!(
                                "Failed to persist hook expiry: {error}"
                            ));
                        }
                    }
                } else if grew {
                    entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
                }
            }
            _ => {}
        }
    }

    /// Forget the output baseline whenever no authoritative hook runtime owns
    /// the Session. If hooks later latch, their first sample starts fresh.
    pub fn clear_output_baseline(&mut self, session_id: &str) {
        let Some(entry) = self.entries.get_mut(session_id) else {
            return;
        };
        entry.last_signal = None;
    }

    /// Re-latch from the durable seed each hook script writes
    /// (`last-hook-event.json`), including missed live deliveries. Output can
    /// extend a recovered opening event's lease, never its event timestamp:
    /// using an output timestamp for ordering can suppress a subsequent Stop.
    pub fn seed_from_disk(
        &mut self,
        session_id: &str,
        session_dir: &Path,
        anchor_start_to_output: bool,
        not_before_unix_ms: Option<u64>,
        current_generation: u64,
    ) {
        self.observe_runtime_launch(session_id, current_generation, not_before_unix_ms);
        self.entries
            .entry(session_id.to_owned())
            .or_default()
            .session_dir = Some(session_dir.to_owned());
        self.sync_cancellation_from_disk(session_id, session_dir, current_generation);
        self.sync_background_from_disk(
            session_id,
            session_dir,
            current_generation,
            not_before_unix_ms,
        );
        let seed_path = session_dir.join("last-hook-event.json");
        let Ok(seed) = fs::File::open(&seed_path) else {
            return;
        };
        let Ok(meta) = seed.metadata() else {
            return;
        };
        // Recover a missed POST even after live hooks have latched. A durable
        // event older than the latest accepted live hook cannot roll it back.
        if self
            .entries
            .get(session_id)
            .and_then(|entry| entry.last_transition_at)
            .is_some_and(|at| meta.modified().is_ok_and(|modified| modified <= at))
        {
            return;
        }
        if let Some(not_before) = not_before_unix_ms {
            let seed_millis = meta
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or(0);
            if seed_millis < not_before {
                return;
            }
        }
        // Read metadata and bytes from the same inode across atomic renames.
        // A stat(path) + read(path) pair can timestamp a new Stop as an older
        // Start and reverse the ordering against a live hook.
        if meta.len() > 64 * 1024 {
            return;
        }
        let mut raw = String::new();
        if seed.take(64 * 1024).read_to_string(&mut raw).is_err() {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let Some(name) = value.get("hook_event_name").and_then(|v| v.as_str()) else {
            return;
        };
        let tool_name = value
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let seed_generation = value
            .get("unpeel_runtime_generation")
            .or_else(|| value.get("unpeelRuntimeGeneration"))
            .and_then(serde_json::Value::as_u64);

        let canonical = normalize_event_name(name);
        let seed_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mut lease_at = seed_at;
        let anchor = match canonical.as_str() {
            "UserPromptSubmit" => true,
            "Start" => anchor_start_to_output,
            _ => false,
        };
        if starts_turn(&canonical) && anchor && !self.is_cancelled(session_id) {
            if let Ok(output_meta) = fs::metadata(session_dir.join("output.bin")) {
                if let Ok(output_mtime) = output_meta.modified() {
                    lease_at = lease_at.max(output_mtime);
                }
            }
        }
        let accepted = self.apply_hook_event_for_runtime(
            session_id,
            &canonical,
            tool_name.as_deref(),
            seed_at,
            seed_generation,
            current_generation,
            not_before_unix_ms,
        );
        if accepted && starts_turn(&canonical) {
            let entry = self.entries.get_mut(session_id).unwrap();
            if unpeel_core::hook_assets::hook_turn_expired(session_dir, current_generation, seed_at)
            {
                entry.state = Some(HookState::Idle);
                entry.deadline_at = None;
            } else {
                entry.deadline_at = Some(lease_at + HOOK_IDLE_TIMEOUT);
            }
        }
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.entries.remove(session_id);
    }

    pub fn retain_sessions(&mut self, live: &std::collections::HashSet<String>) {
        self.entries.retain(|id, _| live.contains(id));
    }
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    fn timed_seed(dir: &Path, event: &str, generation: u64, modified: SystemTime) {
        let path = dir.join("last-hook-event.json");
        fs::write(
            &path,
            serde_json::json!({
                "hook_event_name": event, "unpeel_runtime_generation": generation
            })
            .to_string(),
        )
        .unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn recovered_opening_lease_does_not_change_event_ordering() {
        let dir = tempfile::tempdir().unwrap();
        timed_seed(dir.path(), "UserPromptSubmit", 1, at(1000));
        let output = fs::File::create(dir.path().join("output.bin")).unwrap();
        output.set_modified(at(10_000)).unwrap();
        let mut engine = ActivityEngine::default();
        engine.seed_from_disk("s", dir.path(), true, None, 1);
        assert_eq!(
            engine.entries["s"].deadline_at,
            Some(at(10_000) + HOOK_IDLE_TIMEOUT)
        );
        timed_seed(dir.path(), "Stop", 1, at(2000));
        engine.seed_from_disk("s", dir.path(), true, None, 1);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(
            engine.is_completed("s"),
            "a later output timestamp cannot hide a missed Stop"
        );
    }

    #[test]
    fn expired_turn_stays_idle_after_restart_and_redraw_but_new_turns_work() {
        let dir = tempfile::tempdir().unwrap();
        timed_seed(dir.path(), "UserPromptSubmit", 1, at(1000));
        let mut engine = ActivityEngine::default();
        engine.seed_from_disk("s", dir.path(), true, None, 1);
        engine.note_output_and_sweep("s", 1, false, at(1000));
        engine.note_output_and_sweep("s", 2, false, at(1000) + HOOK_IDLE_TIMEOUT);
        assert_eq!(
            engine.hook_owned_state("s"),
            Some(HookState::Idle),
            "a redraw after expiry cannot extend the lease"
        );
        let output = fs::File::create(dir.path().join("output.bin")).unwrap();
        output.set_modified(at(900_000)).unwrap();
        let mut restarted = ActivityEngine::default();
        restarted.seed_from_disk("s", dir.path(), true, None, 1);
        assert_eq!(restarted.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!restarted.is_completed("s"));
        timed_seed(dir.path(), "UserPromptSubmit", 1, at(901_000));
        restarted.seed_from_disk("s", dir.path(), true, None, 1);
        assert_eq!(restarted.hook_owned_state("s"), Some(HookState::Busy));
        timed_seed(dir.path(), "UserPromptSubmit", 2, at(1000));
        restarted.seed_from_disk("s", dir.path(), true, None, 2);
        assert_eq!(
            restarted.hook_owned_state("s"),
            Some(HookState::Busy),
            "expiry cannot cross runtime generations"
        );
    }

    #[test]
    fn idle_backstop_repairs_missing_stop_without_rewriting_known_outcomes() {
        for ending in ["Stop", "StopFailure", "StopCancelled", "Idle"] {
            let mut engine = ActivityEngine::default();
            engine.apply_hook_event("s", "UserPromptSubmit", None, at(1000));
            engine.apply_hook_event("s", ending, None, at(2000));
            engine.apply_hook_event("s", "Idle", None, at(3000));
            assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
            assert_eq!(engine.is_completed("s"), ending == "Stop");
            engine.apply_hook_event("s", "UserPromptSubmit", None, at(4000));
            assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        }
    }

    fn background_marker(dir: &Path, generation: u64, id: &str) -> std::path::PathBuf {
        let directory = dir.join("background-hooks").join(generation.to_string());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{id}.json"));
        fs::write(
            &path,
            serde_json::json!({
                "activity_id": id, "unpeel_runtime_generation": generation
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    #[test]
    fn background_children_outlive_main_stop_and_finish_independently() {
        let dir = tempfile::tempdir().unwrap();
        let first = background_marker(dir.path(), 1, "first");
        let second = background_marker(dir.path(), 1, "second");
        let mut engine = ActivityEngine::default();
        engine.observe_runtime_launch("s", 1, None);
        engine.apply_hook_event("s", "UserPromptSubmit", None, SystemTime::now());
        engine.sync_background_from_disk("s", dir.path(), 1, None);
        engine.apply_hook_event("s", "Stop", None, SystemTime::now());
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        assert!(!engine.is_completed("s"));
        fs::remove_file(first).unwrap();
        engine.sync_background_from_disk("s", dir.path(), 1, None);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        assert!(!engine.is_completed("s"));
        fs::remove_file(second).unwrap();
        engine.sync_background_from_disk("s", dir.path(), 1, None);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(engine.is_completed("s"));
    }

    #[test]
    fn background_children_survive_foreground_cancellation_without_reporting_completion() {
        for native in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let marker = background_marker(dir.path(), 1, "child");
            let mut engine = ActivityEngine::default();
            engine.observe_runtime_launch("s", 1, None);
            engine.apply_hook_event("s", "UserPromptSubmit", None, at(1000));
            engine.sync_background_from_disk("s", dir.path(), 1, None);
            if native {
                engine.apply_hook_event("s", "StopCancelled", None, at(2000));
            } else {
                engine.observe_cancellation(
                    "s",
                    &unpeel_core::hook_cancellation::Cancellation {
                        runtime_generation: 1,
                        cancelled_at: 2000,
                        submitted_at: None,
                    },
                    1,
                );
            }
            assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
            assert!(!engine.is_completed("s"));
            fs::remove_file(marker).unwrap();
            engine.sync_background_from_disk("s", dir.path(), 1, None);
            assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
            assert!(!engine.is_completed("s"));
        }
    }

    #[test]
    fn background_recovery_is_generation_bound_and_does_not_replace_attention() {
        let dir = tempfile::tempdir().unwrap();
        background_marker(dir.path(), 1, "child");
        fs::write(
            dir.path().join("last-hook-event.json"),
            r#"{"hook_event_name":"Stop","unpeel_runtime_generation":1}"#,
        )
        .unwrap();
        let mut engine = ActivityEngine::default();
        engine.seed_from_disk("s", dir.path(), true, None, 1);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        assert!(!engine.is_completed("s"));
        engine.apply_hook_event("s", "PermissionRequest", None, SystemTime::now());
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Attention));
        engine.seed_from_disk("s", dir.path(), true, None, 2);
        assert!(!engine.is_latched("s"));
        assert_eq!(engine.hook_owned_state("s"), None);
    }

    #[test]
    fn missing_background_stop_expires_without_completion_or_rescan_revival() {
        let dir = tempfile::tempdir().unwrap();
        background_marker(dir.path(), 1, "child");
        let now = SystemTime::now();
        let mut engine = ActivityEngine::default();
        engine.observe_runtime_launch("s", 1, None);
        engine.sync_background_from_disk("s", dir.path(), 1, None);
        engine.apply_hook_event("s", "Stop", None, now);
        engine.note_output_and_sweep("s", 10, true, now);
        engine.note_output_and_sweep(
            "s",
            10,
            true,
            now + HOOK_IDLE_TIMEOUT + Duration::from_secs(1),
        );
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!engine.is_completed("s"));
        engine.sync_background_from_disk("s", dir.path(), 1, None);
        engine.note_output_and_sweep(
            "s",
            11,
            true,
            now + HOOK_IDLE_TIMEOUT + Duration::from_secs(2),
        );
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!engine.is_completed("s"));
    }

    #[test]
    fn cancellation_fences_late_hooks_until_a_submitted_new_turn() {
        let mut engine = ActivityEngine::default();
        engine.observe_runtime_launch("s", 1, None);
        engine.apply_hook_event("s", "UserPromptSubmit", None, at(1000));
        let mut cancellation = unpeel_core::hook_cancellation::Cancellation {
            runtime_generation: 1,
            cancelled_at: 2000,
            submitted_at: None,
        };
        engine.observe_cancellation("s", &cancellation, 1);
        for event in ["PermissionRequest", "Stop", "Start", "UserPromptSubmit"] {
            assert!(!engine.apply_hook_event_for_runtime(
                "s",
                event,
                None,
                at(2100),
                Some(1),
                1,
                None
            ));
        }
        engine.note_output_and_sweep("s", 1, true, at(3000));
        engine.note_output_and_sweep("s", 2, true, at(9000));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(engine.is_cancelled("s"));

        cancellation.submitted_at = Some(10000);
        engine.observe_cancellation("s", &cancellation, 1);
        assert!(engine.is_cancelled("s"), "an Enter alone never starts Busy");
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "UserPromptSubmit",
            None,
            at(10100),
            Some(1),
            1,
            None
        ));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        assert!(!engine.is_cancelled("s"));
        engine.observe_cancellation("s", &cancellation, 1);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
    }

    #[test]
    fn fast_opening_hook_can_beat_submission_marker_in_either_order() {
        for cancellation_arrives_first in [false, true] {
            let mut engine = ActivityEngine::default();
            engine.observe_runtime_launch("s", 1, None);
            engine.apply_hook_event("s", "UserPromptSubmit", None, at(1000));
            let mut cancellation = unpeel_core::hook_cancellation::Cancellation {
                runtime_generation: 1,
                cancelled_at: 2000,
                submitted_at: None,
            };
            if cancellation_arrives_first {
                engine.observe_cancellation("s", &cancellation, 1);
            }
            engine.apply_hook_event("s", "UserPromptSubmit", None, at(3100));
            cancellation.submitted_at = Some(3000);
            engine.observe_cancellation("s", &cancellation, 1);
            assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
            assert!(!engine.is_cancelled("s"));
        }
    }

    #[test]
    fn cancellation_never_latches_hookless_sessions_or_crosses_generation() {
        let mut engine = ActivityEngine::default();
        engine.observe_cancellation(
            "s",
            &unpeel_core::hook_cancellation::Cancellation {
                runtime_generation: 1,
                cancelled_at: 2000,
                submitted_at: None,
            },
            1,
        );
        assert!(!engine.is_latched("s"));
        assert_eq!(engine.hook_owned_state("s"), None);
        engine.observe_runtime_launch("s", 2, Some(3000));
        engine.observe_cancellation(
            "s",
            &unpeel_core::hook_cancellation::Cancellation {
                runtime_generation: 1,
                cancelled_at: 2000,
                submitted_at: None,
            },
            2,
        );
        assert!(!engine.is_cancelled("s"));
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "UserPromptSubmit",
            None,
            at(3100),
            Some(2),
            2,
            Some(3000)
        ));
    }

    #[test]
    fn subagent_stop_cannot_complete_parent_turn() {
        let mut engine = ActivityEngine::default();
        engine.apply_hook_event("s", "UserPromptSubmit", None, at(1000));
        engine.apply_hook_event("s", "SubagentStop", None, at(2000));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
    }

    #[test]
    fn native_cancel_settles_without_completion_and_rearms_on_next_opening_hook() {
        let mut engine = ActivityEngine::default();
        engine.apply_hook_event("s", "UserPromptSubmit", None, at(1000));
        engine.apply_hook_event("s", "stop_cancelled", None, at(2000));
        assert!(engine.is_cancelled("s"));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!engine.is_completed("s"));
        engine.apply_hook_event("s", "PermissionRequest", None, at(2100));
        engine.apply_hook_event("s", "Stop", None, at(2200));
        engine.note_output_and_sweep("s", 1, true, at(2300));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!engine.is_completed("s"));
        // Native lifecycle proof works for client stops and auto-wake turns
        // too: a submitted Enter is not required to resume hook authority.
        engine.apply_hook_event("s", "UserPromptSubmit", None, at(3000));
        assert!(!engine.is_cancelled("s"));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        engine.apply_hook_event("s", "Stop", None, at(4000));
        assert!(engine.is_completed("s"));
    }

    #[test]
    fn failure_and_timeout_are_idle_without_successful_completion() {
        let mut engine = ActivityEngine::default();
        engine.apply_hook_event("s", "Stop", None, at(1000));
        assert!(engine.is_completed("s"));
        engine.apply_hook_event("s", "UserPromptSubmit", None, at(2000));
        assert!(!engine.is_completed("s"));
        engine.apply_hook_event("s", "StopFailure", None, at(3000));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!engine.is_completed("s"));
        engine.apply_hook_event("s", "UserPromptSubmit", None, at(4000));
        engine.note_output_and_sweep("s", 0, true, at(4000) + HOOK_IDLE_TIMEOUT);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        assert!(!engine.is_completed("s"));
    }

    #[test]
    fn durable_stop_recovers_a_missed_post_after_live_hooks_have_latched() {
        let dir = std::env::temp_dir().join(format!("unpeel-hook-recovery-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let seed = dir.join("last-hook-event.json");
        fs::write(
            &seed,
            r#"{"hook_event_name":"Stop","unpeel_runtime_generation":1}"#,
        )
        .unwrap();
        fs::File::open(&seed)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(at(2000)))
            .unwrap();
        let mut engine = ActivityEngine::default();
        engine.apply_hook_event_for_runtime(
            "s",
            "UserPromptSubmit",
            None,
            at(1000),
            Some(1),
            1,
            None,
        );
        engine.apply_hook_event_for_runtime("s", "HookSeen", None, at(2500), Some(1), 1, None);
        engine.seed_from_disk("s", &dir, true, None, 1);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        engine.apply_hook_event_for_runtime(
            "s",
            "UserPromptSubmit",
            None,
            at(3000),
            Some(1),
            1,
            None,
        );
        engine.seed_from_disk("s", &dir, true, None, 1);
        assert_eq!(
            engine.hook_owned_state("s"),
            Some(HookState::Busy),
            "old seed cannot roll back a new live hook"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn normalizes_wire_variants() {
        assert_eq!(normalize_event_name("session-start"), "HookSeen");
        assert_eq!(normalize_event_name("session_start"), "HookSeen");
        assert_eq!(normalize_event_name("Start"), "Start");
        assert_eq!(normalize_event_name("UserPromptSubmit"), "UserPromptSubmit");
        assert_eq!(
            normalize_event_name("before_submit_prompt"),
            "UserPromptSubmit"
        );
        assert_eq!(normalize_event_name("SubagentStop"), "HookSeen");
        assert_eq!(normalize_event_name("STOP FAILURE"), "StopFailure");
        assert_eq!(
            normalize_event_name("permission_request"),
            "PermissionRequest"
        );
        assert_eq!(normalize_event_name("Notification"), "Notification");
    }

    #[test]
    fn latch_and_lifecycle() {
        let mut engine = ActivityEngine::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(engine.hook_owned_state("s").is_none());

        engine.apply_hook_event("s", "UserPromptSubmit", None, now);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        engine.apply_hook_event("s", "Stop", None, now);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));

        engine.apply_hook_event("s", "PermissionRequest", None, now);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Attention));

        // Unknown events latch but change nothing.
        engine.apply_hook_event("s2", "Notification", None, now);
        assert!(engine.is_latched("s2"));
        assert!(engine.hook_owned_state("s2").is_none());

        // AskUserQuestion permission prompts are latch-only.
        engine.apply_hook_event("s3", "PermissionRequest", Some("AskUserQuestion"), now);
        assert!(engine.is_latched("s3"));
        assert!(engine.hook_owned_state("s3").is_none());

        // Grok/Claude SessionStart is open/resume, not a turn.
        engine.apply_hook_event("s4", "session_start", None, now);
        assert!(engine.is_latched("s4"));
        assert!(engine.hook_owned_state("s4").is_none());
    }

    #[test]
    fn foreground_identity_edge_resets_latch_but_first_sighting_does_not() {
        let mut engine = ActivityEngine::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        // Engine coming up mid-run: live events latched first, then the first
        // observation sighting must keep that latch.
        engine.apply_hook_event("s", "UserPromptSubmit", None, now);
        engine.observe_foreground_runtime("s", Some("claude:456"));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // Same identity on later ticks is not an edge.
        engine.observe_foreground_runtime("s", Some("claude:456"));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // The observed process going away only records; the latch may still
        // be reporting (Stop after exit) and status stops consulting it.
        engine.observe_foreground_runtime("s", None);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // A NEW observed process is an edge: the old latch cannot speak for it.
        engine.observe_foreground_runtime("s", Some("claude:789"));
        assert!(engine.hook_owned_state("s").is_none());
        assert!(!engine.is_latched("s"));

        // Fresh live events from the new process latch as usual.
        engine.apply_hook_event("s", "Stop", None, now);
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
    }

    #[test]
    fn busy_timeout_rearms_on_growth_and_expires() {
        let mut engine = ActivityEngine::default();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        engine.apply_hook_event("s", "Start", None, t0);

        // Growth inside the window re-arms the deadline.
        engine.note_output_and_sweep("s", 0, true, t0);
        engine.note_output_and_sweep("s", 100, true, t0 + Duration::from_secs(200));
        engine.note_output_and_sweep("s", 200, true, t0 + Duration::from_secs(400));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // No growth past the re-armed deadline expires to idle.
        engine.note_output_and_sweep(
            "s",
            200,
            true,
            t0 + Duration::from_secs(400) + HOOK_IDLE_TIMEOUT,
        );
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
    }

    #[test]
    fn attention_clears_on_growth_unless_disallowed() {
        let mut engine = ActivityEngine::default();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        engine.apply_hook_event("s", "PermissionRequest", None, t0);
        // First observation is the re-baseline (last_signal was reset).
        engine.note_output_and_sweep("s", 100, true, t0 + Duration::from_secs(1));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Attention));
        engine.note_output_and_sweep("s", 150, true, t0 + Duration::from_secs(2));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // Grok-style: growth never clears attention.
        engine.apply_hook_event("g", "PermissionRequest", None, t0);
        engine.note_output_and_sweep("g", 100, false, t0 + Duration::from_secs(1));
        engine.note_output_and_sweep("g", 150, false, t0 + Duration::from_secs(2));
        assert_eq!(engine.hook_owned_state("g"), Some(HookState::Attention));
    }

    #[test]
    fn settled_turns_stay_idle_through_redraws_and_restart_until_an_opening_hook() {
        for event in ["Stop", "StopFailure", "StopCancelled", "Idle"] {
            let dir = tempfile::tempdir().unwrap();
            let t0 = at(1_000_000);
            timed_seed(dir.path(), event, 1, t0);
            for restored in [false, true] {
                let mut engine = ActivityEngine::default();
                if restored {
                    engine.seed_from_disk("s", dir.path(), true, None, 1);
                } else {
                    engine.apply_hook_event("s", "UserPromptSubmit", None, at(900_000));
                    engine.apply_hook_event("s", event, None, t0);
                }
                engine.note_output_and_sweep("s", 100, true, t0);
                // Reproduce delayed redraws inside the old 5–90 second window,
                // including the reported 12-second repaint, and later output.
                for seconds in [2, 5, 12, 89, 91, 301] {
                    engine.note_output_and_sweep(
                        "s",
                        100 + seconds,
                        true,
                        t0 + Duration::from_secs(seconds),
                    );
                    assert_eq!(
                        engine.hook_owned_state("s"),
                        Some(HookState::Idle),
                        "{event}, restored={restored}, redraw after {seconds}s"
                    );
                    assert_eq!(engine.is_completed("s"), event == "Stop");
                }
                engine.apply_hook_event(
                    "s",
                    "UserPromptSubmit",
                    None,
                    t0 + Duration::from_secs(302),
                );
                assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
                assert!(!engine.is_completed("s"));
            }
        }
    }

    #[test]
    fn seed_from_disk_latches_recorded_stop() {
        let dir = std::env::temp_dir().join(format!("unpeel-tui-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("last-hook-event.json"),
            r#"{"hook_event_name":"Stop"}"#,
        )
        .unwrap();
        let mut engine = ActivityEngine::default();
        engine.seed_from_disk("s", &dir, true, None, 0);
        assert!(engine.is_latched("s"));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_generation_resets_old_latch_and_rejects_older_seed() {
        let dir =
            std::env::temp_dir().join(format!("unpeel-tui-generation-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("last-hook-event.json"),
            r#"{"hook_event_name":"Stop","unpeel_runtime_generation":1}"#,
        )
        .unwrap();

        let mut engine = ActivityEngine::default();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        engine.apply_hook_event("s", "Stop", None, old);
        assert!(engine.is_latched("s"));

        // A generation boundary keeps the Session but drops the old runtime's
        // lifecycle. Even if its mtime is new enough, the explicitly tagged
        // generation-one seed cannot latch generation two.
        engine.observe_runtime_launch("s", 2, None);
        assert!(!engine.is_latched("s"));
        engine.seed_from_disk("s", &dir, true, None, 2);
        assert!(!engine.is_latched("s"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn committed_generation_rejects_old_stop_then_accepts_new_start_stop() {
        let mut engine = ActivityEngine::default();
        let launch = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let launch_ms = launch
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        assert!(!engine.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            launch + Duration::from_millis(1),
            Some(1),
            2,
            Some(launch_ms),
        ));
        assert!(!engine.is_latched("s"));
        assert_eq!(engine.hook_owned_state("s"), None);

        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Start",
            None,
            launch + Duration::from_millis(2),
            Some(2),
            2,
            Some(launch_ms),
        ));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            launch + Duration::from_millis(3),
            Some(2),
            2,
            Some(launch_ms),
        ));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
    }

    #[test]
    fn old_stop_received_before_manifest_commit_cannot_survive_generation_edge() {
        let mut engine = ActivityEngine::default();
        let old_launch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let replacement_launch = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let replacement_ms = replacement_launch
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Start",
            None,
            old_launch,
            Some(1),
            1,
            Some(1_000_000),
        ));
        // The old reporter reaches the TUI after the replacement launch time,
        // but while the manifest still says generation one. Its exact tag is
        // retained, so observing committed generation two resets it anyway.
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            replacement_launch + Duration::from_millis(1),
            Some(1),
            1,
            Some(1_000_000),
        ));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
        engine.observe_runtime_launch("s", 2, Some(replacement_ms));
        assert!(!engine.is_latched("s"));
        assert_eq!(engine.hook_owned_state("s"), None);
    }

    #[test]
    fn legacy_start_before_manifest_commit_rebinds_to_replacement_generation() {
        let mut engine = ActivityEngine::default();
        let replacement_launch = SystemTime::UNIX_EPOCH + Duration::from_secs(4_000);
        let replacement_ms = replacement_launch
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // An old installed hook cannot tag this Start as generation two. It
        // can still beat the Host manifest commit, when the listener sees one.
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Start",
            None,
            replacement_launch + Duration::from_millis(1),
            None,
            1,
            Some(3_000_000),
        ));
        engine.observe_runtime_launch("s", 2, Some(replacement_ms));
        assert_eq!(engine.runtime_launch_generation("s"), Some(2));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // The rebound opener now proves an untagged Stop belongs to gen2.
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            replacement_launch + Duration::from_millis(2),
            None,
            2,
            Some(replacement_ms),
        ));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));
    }

    #[test]
    fn legacy_stop_after_generation_edge_waits_for_current_start_but_is_bounded() {
        let launch = SystemTime::UNIX_EPOCH + Duration::from_secs(3_000);
        let launch_ms = launch
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mut engine = ActivityEngine::default();

        assert!(!engine.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            launch + Duration::from_secs(1),
            None,
            2,
            Some(launch_ms),
        ));
        assert!(!engine.is_latched("s"));

        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "UserPromptSubmit",
            None,
            launch + Duration::from_secs(2),
            None,
            2,
            Some(launch_ms),
        ));
        assert!(engine.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            launch + Duration::from_secs(3),
            None,
            2,
            Some(launch_ms),
        ));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));

        let mut never_started = ActivityEngine::default();
        assert!(never_started.apply_hook_event_for_runtime(
            "s",
            "Stop",
            None,
            launch + LEGACY_GENERATION_STOP_GUARD,
            None,
            2,
            Some(launch_ms),
        ));
    }
}
