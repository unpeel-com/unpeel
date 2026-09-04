//! Port of the native busy/idle/attention engine (`SessionActivity.swift`).
//! Hook-capable tools are hook-owned: the first hook event latches the
//! session, and from then on only hook events and the 5-minute
//! output-rearmed timeout change its state — raw output growth never flips
//! busy/idle directly. Hookless agents and ordinary terminal programs remain
//! visually neutral because output/screen changes have no lifecycle authority.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

pub const HOOK_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Stop-distrust guard (codex): codex fires agent-turn-complete Stops for
/// internal sub-turns of one long run, so a Stop is not proof the work ended.
/// Growth observed after the grace (past the turn's trailing render burst)
/// but inside the window re-arms busy; the bounded window keeps later user
/// scroll repaints from faking busy on a genuinely finished session.
const STOP_REARM_GRACE: Duration = Duration::from_secs(5);
const STOP_REARM_WINDOW: Duration = Duration::from_secs(90);
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
    /// When the latest Stop/StopFailure landed; the stop-distrust guard only
    /// re-arms busy inside [grace, window] after this instant.
    stopped_at: Option<SystemTime>,
    /// Timestamp of the newest live/durable hook folded into this entry.
    /// Lets a hook from the new generation win a rescan race with the
    /// manifest generation update instead of being cleared as stale.
    last_hook_at: Option<SystemTime>,
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
        "stop" | "sessionend" | "subagentstop" => "Stop".into(),
        "stopfailure" => "StopFailure".into(),
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
        "Start" | "UserPromptSubmit" | "Stop" | "StopFailure" => false,
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
    ) {
        let canonical = normalize_event_name(raw_name);
        let latch_only = is_latch_only(&canonical, tool_name);
        let entry = self.entries.entry(session_id.to_string()).or_default();
        entry.hook_seen = true;
        entry.last_hook_at = Some(now);
        if latch_only {
            return;
        }
        match canonical.as_str() {
            "Start" | "UserPromptSubmit" => {
                entry.state = Some(HookState::Busy);
                entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
                entry.stopped_at = None;
            }
            "Stop" | "StopFailure" => {
                entry.state = Some(HookState::Idle);
                entry.deadline_at = None;
                entry.stopped_at = Some(now);
            }
            "PermissionRequest" => {
                entry.state = Some(HookState::Attention);
                entry.deadline_at = None;
                entry.stopped_at = None;
                // Re-baseline output tracking so the answer's redraw counts
                // as fresh growth.
                entry.last_signal = None;
            }
            _ => {}
        }
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
        } else if matches!(canonical.as_str(), "Stop" | "StopFailure")
            && event_generation.is_none()
            && current_generation > 1
            && entry.confirmed_turn_generation != Some(current_generation)
            && launched_at.is_some_and(|launch| {
                now.duration_since(launch).unwrap_or_default() < LEGACY_GENERATION_STOP_GUARD
            })
        {
            return false;
        }

        self.apply_hook_event_unchecked(session_id, &canonical, tool_name, now);
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
        entry.state
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
    /// `distrust_stops` is true for tools that fire Stop mid-run (codex): a
    /// hook-idle session whose output keeps growing past the stop-rearm grace
    /// flips back to busy.
    pub fn note_output_and_sweep(
        &mut self,
        session_id: &str,
        activity_signal: u64,
        allow_attention_clear: bool,
        distrust_stops: bool,
        now: SystemTime,
    ) {
        let entry = self.entries.entry(session_id.to_string()).or_default();
        let grew = entry
            .last_signal
            .is_some_and(|previous| previous != activity_signal);
        entry.last_signal = Some(activity_signal);
        match entry.state {
            Some(HookState::Attention) if allow_attention_clear && grew => {
                entry.state = Some(HookState::Busy);
                entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
            }
            Some(HookState::Busy) => {
                if grew {
                    entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
                } else if entry.deadline_at.is_some_and(|d| d <= now) {
                    entry.state = Some(HookState::Idle);
                    entry.deadline_at = None;
                }
            }
            Some(HookState::Idle) if distrust_stops && grew => {
                if let Some(stopped_at) = entry.stopped_at {
                    let since_stop = now.duration_since(stopped_at).unwrap_or_default();
                    if since_stop >= STOP_REARM_GRACE && since_stop <= STOP_REARM_WINDOW {
                        entry.state = Some(HookState::Busy);
                        entry.deadline_at = Some(now + HOOK_IDLE_TIMEOUT);
                    }
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
    /// (`last-hook-event.json`). Only called when the in-memory latch is
    /// missing (fresh TUI start). Timestamp anchoring mirrors
    /// `UnpeelStore.seedHookActivity`: a turn-opening event is anchored at
    /// `max(seed mtime, output.bin mtime)` because long turns outlive the
    /// 5-minute timeout; everything else uses the seed's own mtime.
    pub fn seed_from_disk(
        &mut self,
        session_id: &str,
        session_dir: &Path,
        anchor_start_to_output: bool,
        not_before_unix_ms: Option<u64>,
        current_generation: u64,
    ) {
        if self.is_latched(session_id) {
            return;
        }
        let seed_path = session_dir.join("last-hook-event.json");
        let Ok(meta) = fs::metadata(&seed_path) else {
            return;
        };
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
        let Ok(raw) = fs::read_to_string(&seed_path) else {
            return;
        };
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
        let mut seed_at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let anchor = match canonical.as_str() {
            "UserPromptSubmit" => true,
            "Start" => anchor_start_to_output,
            _ => false,
        };
        if starts_turn(&canonical) && anchor {
            if let Ok(output_meta) = fs::metadata(session_dir.join("output.bin")) {
                if let Ok(output_mtime) = output_meta.modified() {
                    seed_at = seed_at.max(output_mtime);
                }
            }
        }
        self.apply_hook_event_for_runtime(
            session_id,
            &canonical,
            tool_name.as_deref(),
            seed_at,
            seed_generation,
            current_generation,
            not_before_unix_ms,
        );
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.entries.remove(session_id);
    }

    pub fn retain_sessions(&mut self, live: &std::collections::HashSet<String>) {
        self.entries.retain(|id, _| live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(normalize_event_name("SubagentStop"), "Stop");
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
        engine.note_output_and_sweep("s", 100, true, false, t0 + Duration::from_secs(200));
        engine.note_output_and_sweep("s", 200, true, false, t0 + Duration::from_secs(400));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // No growth past the re-armed deadline expires to idle.
        engine.note_output_and_sweep(
            "s",
            200,
            true,
            false,
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
        engine.note_output_and_sweep("s", 100, true, false, t0 + Duration::from_secs(1));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Attention));
        engine.note_output_and_sweep("s", 150, true, false, t0 + Duration::from_secs(2));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // Grok-style: growth never clears attention.
        engine.apply_hook_event("g", "PermissionRequest", None, t0);
        engine.note_output_and_sweep("g", 100, false, false, t0 + Duration::from_secs(1));
        engine.note_output_and_sweep("g", 150, false, false, t0 + Duration::from_secs(2));
        assert_eq!(engine.hook_owned_state("g"), Some(HookState::Attention));
    }

    #[test]
    fn distrusted_stop_rearms_busy_on_sustained_growth_only() {
        let mut engine = ActivityEngine::default();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        engine.apply_hook_event("s", "UserPromptSubmit", None, t0);
        engine.note_output_and_sweep("s", 100, true, true, t0 + Duration::from_secs(1));
        engine.apply_hook_event("s", "Stop", None, t0 + Duration::from_secs(10));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));

        // The turn's trailing render burst lands inside the grace: stays idle.
        engine.note_output_and_sweep("s", 200, true, true, t0 + Duration::from_secs(12));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));

        // Sustained growth past the grace re-arms busy (codex mid-run Stop).
        engine.note_output_and_sweep("s", 300, true, true, t0 + Duration::from_secs(17));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Busy));

        // Growth outside the window (a later scroll repaint) never re-arms.
        engine.apply_hook_event("s", "Stop", None, t0 + Duration::from_secs(30));
        engine.note_output_and_sweep("s", 300, true, true, t0 + Duration::from_secs(31));
        engine.note_output_and_sweep("s", 400, true, true, t0 + Duration::from_secs(200));
        assert_eq!(engine.hook_owned_state("s"), Some(HookState::Idle));

        // Providers without the guard keep the strict hook latch.
        engine.apply_hook_event("c", "Stop", None, t0);
        engine.note_output_and_sweep("c", 100, true, false, t0 + Duration::from_secs(1));
        engine.note_output_and_sweep("c", 200, true, false, t0 + Duration::from_secs(10));
        assert_eq!(engine.hook_owned_state("c"), Some(HookState::Idle));
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
