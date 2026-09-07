//! Grok CLI usage. The existing `~/.grok/auth.json` login supplies the live
//! weekly shared pool, Extra Usage status, and plan. Completed turns under
//! `~/.grok/sessions/**/updates.jsonl` supply machine-local token and recorded
//! cost history. Credential values never enter the provider model or UI.

use crate::config::Config;
use crate::sources::{
    aggregate_monthly_tokens, aggregate_project_usage, compact_tokens, compact_usd, Level, Metric,
    Provider, ProviderKind,
};
use crate::timeparse::{compact_duration, now_epoch_secs, parse_epoch_secs};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ureq::http;

const DAY_SECS: i64 = 24 * 3_600;
const HISTORY_DAYS: usize = 30;
const LOOKBACK_SECS: i64 = 32 * DAY_SECS;
const MONTH_HISTORY_SECS: i64 = 400 * DAY_SECS;
const REMOTE_CACHE_SECS: i64 = 5 * 60;
const REMOTE_RETRY_SECS: i64 = 60;
const REFRESH_BUFFER_SECS: i64 = 5 * 60;
const MAXIMUM_PLAUSIBLE_TOKENS: u64 = 1_000_000_000_000;
const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
const REFRESH_URL: &str = "https://auth.x.ai/oauth2/token";

#[derive(Clone, Default)]
struct RemoteOutcome {
    weekly: Option<WeeklyQuota>,
    /// `Some(0)` means the live response explicitly says Extra Usage is off.
    extra_cap: Option<f64>,
    plan: Option<String>,
    fetched_at: Option<i64>,
    warning: Option<String>,
}

#[derive(Clone)]
struct WeeklyQuota {
    used: f64,
    starts_at: i64,
    resets_at: i64,
}

#[derive(Clone)]
struct CachedRemote {
    outcome: RemoteOutcome,
    retry_at: i64,
}

fn remote_cache() -> &'static Mutex<HashMap<PathBuf, CachedRemote>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedRemote>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Debug)]
struct LogEntry {
    event_id: Option<String>,
    at: i64,
    model: String,
    tokens: u64,
    cost: Option<f64>,
    project: Option<PathBuf>,
}

#[derive(Clone)]
struct CachedLog {
    len: u64,
    modified: Option<SystemTime>,
    entries: Vec<LogEntry>,
    /// Bytes after the last newline. They are joined with the next append so
    /// an in-progress JSON object is never parsed as a completed turn.
    pending: Vec<u8>,
}

fn log_cache() -> &'static Mutex<HashMap<PathBuf, CachedLog>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedLog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Copy, Default)]
struct Totals {
    cost: f64,
    cost_rows: u64,
    missing_cost_rows: u64,
    tokens: u64,
}

struct History {
    daily: Vec<Totals>,
    entries: Vec<LogEntry>,
    starts: Vec<i64>,
    latest: Option<i64>,
}

pub fn scan(config: &Config) -> Provider {
    let mut provider = Provider {
        kind: ProviderKind::Grok,
        name: "Grok".into(),
        badge: String::new(),
        present: false,
        metrics: Vec::new(),
        detail: Vec::new(),
        as_of: None,
        alert: None,
        status_fragment: None,
        day_usd: None,
        monthly_tokens: Vec::new(),
        project_usage: Vec::new(),
    };
    let Some(home) = grok_home() else {
        return provider;
    };
    if !home.is_dir() {
        return provider;
    }
    provider.present = true;

    let now = now_epoch_secs();
    let history = scan_history(&home, now);
    provider.monthly_tokens =
        aggregate_monthly_tokens(history.entries.iter().map(|entry| (entry.at, entry.tokens)));
    provider.project_usage = aggregate_project_usage(
        history
            .entries
            .iter()
            .map(|entry| (entry.at, entry.tokens, entry.project.clone())),
    );
    let remote = if config.grok.live_usage {
        cached_remote_usage(&home, now)
    } else {
        RemoteOutcome {
            warning: Some("disabled in config; local session history is still available".into()),
            ..RemoteOutcome::default()
        }
    };

    provider.badge = remote.plan.unwrap_or_default();
    provider.as_of = newest(remote.fetched_at, history.latest);
    if let Some(warning) = remote.warning {
        provider.detail.push(("live usage".into(), warning));
    }

    if let Some(quota) = remote.weekly {
        let used = quota.used.clamp(0.0, 100.0);
        let reset = if quota.resets_at > now {
            format!(" · resets {}", compact_duration(quota.resets_at - now))
        } else {
            String::new()
        };
        let level = usage_level(used);
        let mut metric = Metric::used_percent("Weekly", used, format!("{used:.0}%{reset}"), level);
        let period = quota.resets_at.saturating_sub(quota.starts_at);
        if period > 0 {
            metric.marker = Some(
                ((now.saturating_sub(quota.starts_at)) as f64 / period as f64).clamp(0.0, 1.0),
            );
        }
        if used >= 99.5 {
            metric.annotation = Some("Limit reached".into());
        }
        if level == Level::Alert {
            provider.alert = Some(format!("Grok weekly at {used:.0}%"));
        }
        provider.status_fragment = Some(format!("Grok {used:.0}%"));
        provider.metrics.push(metric);
    } else {
        provider
            .metrics
            .push(Metric::new("Weekly", "No data".into(), Level::Ok));
    }

    provider.metrics.push(Metric::new(
        "Extra Usage",
        match remote.extra_cap {
            Some(cap) if cap > 0.0 => format!("{} cap", compact_units(cap)),
            Some(_) => "Disabled".into(),
            None => "No data".into(),
        },
        Level::Ok,
    ));

    append_history(&mut provider, &history, now);
    provider
}

fn newest(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn usage_level(used: f64) -> Level {
    if used >= 90.0 {
        Level::Alert
    } else if used >= 80.0 {
        Level::Warn
    } else {
        Level::Ok
    }
}

fn compact_units(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn grok_home() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("GROK_HOME") {
        let raw = raw.trim();
        if !raw.is_empty() {
            if let Some(rest) = raw.strip_prefix("~/") {
                return std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(rest));
            }
            return Some(PathBuf::from(raw));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".grok"))
}

// MARK: Live billing

fn cached_remote_usage(home: &Path, now: i64) -> RemoteOutcome {
    let key = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    {
        let cache = remote_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.get(&key) {
            if now < cached.retry_at {
                return cached.outcome.clone();
            }
        }
    }

    let outcome = fetch_remote_usage(&home.join("auth.json"), now);
    let cache_for = if outcome.fetched_at.is_some() {
        REMOTE_CACHE_SECS
    } else {
        REMOTE_RETRY_SECS
    };
    remote_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            key,
            CachedRemote {
                outcome: outcome.clone(),
                retry_at: now + cache_for,
            },
        );
    outcome
}

#[derive(Clone)]
struct AuthCandidate {
    entry_key: String,
    token: String,
    refresh_token: Option<String>,
    client_id: String,
    entry_expiry: Option<i64>,
}

impl AuthCandidate {
    fn needs_refresh(&self, now: i64) -> bool {
        [self.entry_expiry, jwt_expiry(&self.token)]
            .into_iter()
            .flatten()
            .any(|expiry| expiry <= now + REFRESH_BUFFER_SECS)
    }

    fn is_expired(&self, now: i64) -> bool {
        jwt_expiry(&self.token)
            .or(self.entry_expiry)
            .is_some_and(|expiry| expiry <= now)
    }
}

fn load_auth_candidates(path: &Path) -> Result<Vec<AuthCandidate>, ()> {
    let raw = std::fs::read_to_string(path).map_err(|_| ())?;
    let value: Value = serde_json::from_str(&raw).map_err(|_| ())?;
    let object = value.as_object().ok_or(())?;
    let mut candidates = Vec::new();
    for (entry_key, raw_entry) in object {
        let Some(entry) = raw_entry.as_object() else {
            continue;
        };
        let Some(token) = trimmed_string(entry.get("key")) else {
            continue;
        };
        let refresh_token = trimmed_string(entry.get("refresh_token"))
            .or_else(|| trimmed_string(entry.get("refresh")));
        let client_id = trimmed_string(entry.get("oidc_client_id"))
            .or_else(|| {
                entry_key
                    .rsplit_once("::")
                    .map(|(_, candidate)| candidate.trim().to_string())
                    .filter(|candidate| !candidate.is_empty())
            })
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
        let entry_expiry = entry
            .get("expires_at")
            .or_else(|| entry.get("expires"))
            .and_then(epoch_value);
        candidates.push(AuthCandidate {
            entry_key: entry_key.clone(),
            token,
            refresh_token,
            client_id,
            entry_expiry,
        });
    }
    (!candidates.is_empty()).then_some(candidates).ok_or(())
}

fn trimmed_string(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn epoch_value(value: &Value) -> Option<i64> {
    if let Some(raw) = value.as_str() {
        return parse_epoch_secs(raw).or_else(|| raw.parse::<i64>().ok());
    }
    let number = value.as_f64()?;
    number.is_finite().then(|| {
        if number.abs() >= 10_000_000_000.0 {
            (number / 1_000.0).round() as i64
        } else {
            number.round() as i64
        }
    })
}

fn fetch_remote_usage(path: &Path, now: i64) -> RemoteOutcome {
    let Ok(candidates) = load_auth_candidates(path) else {
        return RemoteOutcome {
            warning: Some("run `grok login` for live Weekly and Extra Usage".into()),
            ..RemoteOutcome::default()
        };
    };
    let mut saw_expired = false;
    for mut candidate in candidates {
        if candidate.needs_refresh(now)
            && refresh_candidate(path, &mut candidate, now).is_err()
            && candidate.is_expired(now)
        {
            saw_expired = true;
            continue;
        }

        let mut response = get_with_token(BILLING_URL, &candidate.token);
        if response
            .as_ref()
            .is_ok_and(|response| matches!(response.status, 401 | 403))
            && refresh_candidate(path, &mut candidate, now).is_ok()
        {
            response = get_with_token(BILLING_URL, &candidate.token);
        }
        let response = match response {
            Ok(response) => response,
            Err(()) => {
                return remote_warning(
                    "Grok billing is temporarily unavailable; local history is still available",
                );
            }
        };
        if matches!(response.status, 401 | 403) {
            saw_expired = true;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return remote_warning(&format!(
                "Grok billing returned HTTP {}; local history is still available",
                response.status
            ));
        }
        let Some(billing) = parse_billing(&response.body) else {
            return remote_warning(
                "Grok billing response changed; local history is still available",
            );
        };
        let plan = get_with_token(SETTINGS_URL, &candidate.token)
            .ok()
            .filter(|response| (200..300).contains(&response.status))
            .and_then(|response| parse_plan(&response.body));
        return RemoteOutcome {
            weekly: billing.weekly,
            extra_cap: Some(billing.extra_cap),
            plan,
            fetched_at: Some(now),
            warning: None,
        };
    }
    remote_warning(if saw_expired {
        "Grok login expired; run `grok login` again"
    } else {
        "Grok login is invalid; run `grok login` again"
    })
}

fn remote_warning(message: &str) -> RemoteOutcome {
    RemoteOutcome {
        warning: Some(message.into()),
        ..RemoteOutcome::default()
    }
}

struct Billing {
    weekly: Option<WeeklyQuota>,
    extra_cap: f64,
}

fn parse_billing(body: &str) -> Option<Billing> {
    let body: Value = serde_json::from_str(body).ok()?;
    let config = body.get("config")?.as_object()?;
    let period = config.get("currentPeriod")?.as_object()?;
    let period_type = period.get("type")?.as_str()?.trim();
    let starts_at = period.get("start").and_then(epoch_value)?;
    let resets_at = period.get("end").and_then(epoch_value)?;
    if period_type.is_empty() || resets_at <= starts_at {
        return None;
    }
    let used = match config.get("creditUsagePercent") {
        Some(value) => number(value).filter(|value| value.is_finite())?,
        None => 0.0,
    };
    let extra_cap = match config.get("onDemandCap") {
        Some(value) => match value.as_object()?.get("val") {
            Some(value) => number(value).filter(|value| value.is_finite() && *value >= 0.0)?,
            None => 0.0,
        },
        None => 0.0,
    };
    let weekly = (period_type == "USAGE_PERIOD_TYPE_WEEKLY").then(|| WeeklyQuota {
        used: used.clamp(0.0, 100.0),
        starts_at,
        resets_at,
    });
    Some(Billing { weekly, extra_cap })
}

fn parse_plan(body: &str) -> Option<String> {
    let body: Value = serde_json::from_str(body).ok()?;
    trimmed_string(body.get("subscription_tier_display"))
}

struct RawResponse {
    status: u16,
    body: String,
}

fn http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .new_agent()
    })
}

fn get_with_token(url: &str, token: &str) -> Result<RawResponse, ()> {
    let authorization = format!("Bearer {}", token.trim());
    let response = http_agent()
        .get(url)
        .header("Authorization", authorization)
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            concat!("unpeel-usage/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|_| ())?;
    read_response(response)
}

fn read_response(mut response: http::Response<ureq::Body>) -> Result<RawResponse, ()> {
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().map_err(|_| ())?;
    Ok(RawResponse { status, body })
}

#[derive(Default)]
struct RefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<i64>,
}

fn refresh_candidate(path: &Path, candidate: &mut AuthCandidate, now: i64) -> Result<(), ()> {
    let refresh_token = candidate.refresh_token.as_deref().ok_or(())?;
    let response = http_agent()
        .post(REFRESH_URL)
        .send_form([
            ("grant_type", "refresh_token"),
            ("client_id", candidate.client_id.as_str()),
            ("refresh_token", refresh_token),
        ])
        .map_err(|_| ())?;
    let response = read_response(response)?;
    if !(200..300).contains(&response.status) {
        return Err(());
    }
    let body: Value = serde_json::from_str(&response.body).map_err(|_| ())?;
    let access_token = trimmed_string(body.get("access_token")).ok_or(())?;
    let refresh_token = trimmed_string(body.get("refresh_token"));
    let id_token = trimmed_string(body.get("id_token"));
    let expires_at = body
        .get("expires_in")
        .and_then(number)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|seconds| now.saturating_add(seconds.round() as i64))
        .or_else(|| jwt_expiry(&access_token))
        .or(Some(now + 3_600));
    let refreshed = RefreshResponse {
        access_token: access_token.clone(),
        refresh_token: refresh_token.clone(),
        id_token,
        expires_at,
    };
    candidate.token = access_token;
    if let Some(refresh_token) = refresh_token {
        candidate.refresh_token = Some(refresh_token);
    }
    candidate.entry_expiry = expires_at;
    // The refreshed token remains valid for this scan even if a concurrent
    // Grok write or filesystem error prevents safe persistence.
    let _ = persist_refreshed_auth(path, &candidate.entry_key, &refreshed);
    Ok(())
}

fn persist_refreshed_auth(
    path: &Path,
    entry_key: &str,
    refreshed: &RefreshResponse,
) -> Result<(), ()> {
    #[cfg(unix)]
    let _lock = AuthFileLock::acquire(&path.with_extension("json.lock"))?;

    let raw = std::fs::read_to_string(path).map_err(|_| ())?;
    let mut root: Value = serde_json::from_str(&raw).map_err(|_| ())?;
    let entry = root
        .as_object_mut()
        .and_then(|object| object.get_mut(entry_key))
        .and_then(Value::as_object_mut)
        .ok_or(())?;
    entry.insert("key".into(), Value::String(refreshed.access_token.clone()));
    if let Some(token) = &refreshed.refresh_token {
        entry.insert("refresh_token".into(), Value::String(token.clone()));
    }
    if let Some(token) = &refreshed.id_token {
        entry.insert("id_token".into(), Value::String(token.clone()));
    }
    if let Some(expiry) = refreshed.expires_at {
        entry.insert("expires_at".into(), Value::String(format_utc_epoch(expiry)));
    }

    let parent = path.parent().ok_or(())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let temporary = parent.join(format!(".auth.json.tmp-{}-{nonce}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|_| ())?;
    let mut encoded = serde_json::to_vec_pretty(&root).map_err(|_| ())?;
    encoded.push(b'\n');
    let result = file
        .write_all(&encoded)
        .and_then(|_| file.sync_all())
        .and_then(|_| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(());
    }
    Ok(())
}

#[cfg(unix)]
struct AuthFileLock(std::fs::File);

#[cfg(unix)]
impl AuthFileLock {
    fn acquire(path: &Path) -> Result<Self, ()> {
        use std::os::fd::AsRawFd as _;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|_| ())?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        (result == 0).then_some(Self(file)).ok_or(())
    }
}

#[cfg(unix)]
impl Drop for AuthFileLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd as _;
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn jwt_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let decoded = decode_base64_url(payload)?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value.get("exp").and_then(epoch_value)
}

fn decode_base64_url(raw: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(raw.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u8;
    for byte in raw.bytes() {
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((accumulator >> bits) as u8);
            accumulator &= (1u32 << bits).saturating_sub(1);
        }
    }
    Some(output)
}

fn format_utc_epoch(epoch: i64) -> String {
    let days = epoch.div_euclid(DAY_SECS);
    let seconds = epoch.rem_euclid(DAY_SECS);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3_600;
    let minute = seconds % 3_600 / 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's inverse civil-date conversion.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())
}

// MARK: Local session history

fn scan_history(home: &Path, now: i64) -> History {
    let starts = calendar_starts(now, HISTORY_DAYS);
    let since = now.saturating_sub(MONTH_HISTORY_SECS);
    let mut files = Vec::new();
    collect_update_files(&home.join("sessions"), since, &mut files);
    files.sort();
    let visible: HashSet<PathBuf> = files.iter().cloned().collect();
    log_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|path, _| visible.contains(path));

    let mut entries = Vec::new();
    for path in files {
        entries.extend(cached_log_entries(&path));
    }
    let mut seen = HashSet::new();
    entries.retain(|entry| {
        entry
            .event_id
            .as_ref()
            .is_none_or(|event_id| seen.insert(format!("{event_id}\0{}", entry.model)))
    });
    entries.retain(|entry| entry.at >= since);
    entries.sort_by_key(|entry| entry.at);
    let latest = entries.last().map(|entry| entry.at);
    let daily = aggregate_days(&entries, &starts);
    History {
        daily,
        entries,
        starts,
        latest,
    }
}

fn collect_update_files(directory: &Path, since: i64, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_update_files(&path, since, output);
            continue;
        }
        if !file_type.is_file()
            || path.file_name().and_then(|name| name.to_str()) != Some("updates.jsonl")
            || path_has_component(&path, "subagents")
            || is_subagent_session(&path)
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let recent_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .is_none_or(|modified| modified.as_secs() as i64 >= since);
        if recent_enough {
            output.push(path);
        }
    }
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(value) if value == std::ffi::OsStr::new(expected))
    })
}

fn is_subagent_session(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(parent.join("summary.json")) else {
        return false;
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|summary| trimmed_string(summary.get("session_kind")))
        .is_some_and(|kind| kind.to_ascii_lowercase().starts_with("subagent"))
}

fn cached_log_entries(path: &Path) -> Vec<LogEntry> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let len = metadata.len();
    let modified = metadata.modified().ok();
    let previous = log_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .cloned();
    if let Some(cached) = &previous {
        if cached.len == len && cached.modified == modified {
            return cached.entries.clone();
        }
    }

    let updated = if let Some(cached) = previous.as_ref().filter(|cached| len > cached.len) {
        append_log_cache(path, modified, cached)
    } else {
        rebuild_log_cache(path, modified)
    };
    let Some(mut updated) = updated else {
        return previous.map(|cached| cached.entries).unwrap_or_default();
    };
    let project = session_project(path);
    for entry in &mut updated.entries {
        entry.project = project.clone();
    }
    let entries = updated.entries.clone();
    log_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), updated);
    entries
}

fn session_project(path: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(path.parent()?.join("summary.json")).ok()?;
    let summary = serde_json::from_str::<Value>(&raw).ok()?;
    summary
        .pointer("/info/cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(PathBuf::from)
}

fn append_log_cache(
    path: &Path,
    modified: Option<SystemTime>,
    cached: &CachedLog,
) -> Option<CachedLog> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(cached.len)).ok()?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended).ok()?;
    let mut combined = cached.pending.clone();
    combined.extend_from_slice(&appended);
    let (new_entries, pending) = parse_complete_lines(&combined);
    let mut entries = cached.entries.clone();
    entries.extend(new_entries);
    Some(CachedLog {
        len: cached.len.saturating_add(appended.len() as u64),
        modified,
        entries,
        pending,
    })
}

fn rebuild_log_cache(path: &Path, modified: Option<SystemTime>) -> Option<CachedLog> {
    let data = std::fs::read(path).ok()?;
    let (entries, pending) = parse_complete_lines(&data);
    Some(CachedLog {
        len: data.len() as u64,
        modified,
        entries,
        pending,
    })
}

fn parse_complete_lines(data: &[u8]) -> (Vec<LogEntry>, Vec<u8>) {
    let complete_end = data
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut entries = Vec::new();
    for line in data[..complete_end].split(|byte| *byte == b'\n') {
        if !line
            .windows(b"turn_completed".len())
            .any(|window| window == b"turn_completed")
        {
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(line) {
            entries.extend(parse_completed_turn(&value));
        }
    }
    (entries, data[complete_end..].to_vec())
}

fn parse_completed_turn(object: &Value) -> Vec<LogEntry> {
    let params = object.get("params");
    let update = params
        .and_then(|params| params.get("update"))
        .or_else(|| object.get("update"));
    if update
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
        != Some("turn_completed")
    {
        return Vec::new();
    }
    let Some(usage) = update.and_then(|update| update.get("usage")) else {
        return Vec::new();
    };
    let Some(models) = usage.get("modelUsage").and_then(Value::as_object) else {
        return Vec::new();
    };
    let Some(at) = turn_timestamp(object, params) else {
        return Vec::new();
    };
    let metadata = params
        .and_then(|params| params.get("_meta"))
        .or_else(|| object.get("_meta"));
    let event_id = metadata.and_then(|metadata| trimmed_string(metadata.get("eventId")));
    let top_cost = cost_from_ticks(usage.get("costUsdTicks"));
    let mut rows: Vec<(&String, &Value)> = models.iter().collect();
    rows.sort_by(|left, right| left.0.cmp(right.0));
    rows.into_iter()
        .filter_map(|(raw_model, values)| {
            let model = raw_model.trim();
            let values = values.as_object()?;
            let input = bounded_tokens(values.get("inputTokens")?)?;
            let output = bounded_tokens_or_zero(values.get("outputTokens"));
            let cost = cost_from_ticks(values.get("costUsdTicks"))
                .or_else(|| (models.len() == 1).then_some(top_cost).flatten());
            Some(LogEntry {
                event_id: event_id.clone(),
                at,
                model: model.to_string(),
                tokens: input.saturating_add(output),
                cost,
                project: None,
            })
        })
        .filter(|entry| !entry.model.is_empty())
        .collect()
}

fn turn_timestamp(object: &Value, params: Option<&Value>) -> Option<i64> {
    for metadata in [
        params.and_then(|params| params.get("_meta")),
        object.get("_meta"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(milliseconds) = metadata.get("agentTimestampMs").and_then(number) {
            if milliseconds.is_finite() && milliseconds > 0.0 {
                return Some((milliseconds / 1_000.0).round() as i64);
            }
        }
    }
    let timestamp = object.get("timestamp")?;
    if let Some(raw) = timestamp.as_str() {
        return parse_epoch_secs(raw);
    }
    let number = number(timestamp)?;
    (number.is_finite() && number > 0.0).then(|| {
        if number >= 10_000_000_000.0 {
            (number / 1_000.0).round() as i64
        } else {
            number.round() as i64
        }
    })
}

fn bounded_tokens(value: &Value) -> Option<u64> {
    let number = number(value)?;
    (number.is_finite() && number >= 0.0)
        .then(|| number.min(MAXIMUM_PLAUSIBLE_TOKENS as f64).round() as u64)
}

fn bounded_tokens_or_zero(value: Option<&Value>) -> u64 {
    value.and_then(bounded_tokens).unwrap_or(0)
}

fn cost_from_ticks(value: Option<&Value>) -> Option<f64> {
    let ticks = value.and_then(number)?;
    (ticks.is_finite() && ticks >= 0.0).then(|| ticks / 10_000_000_000.0)
}

#[cfg(unix)]
fn local_day_start(epoch: i64) -> i64 {
    let timestamp = epoch as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return epoch - epoch.rem_euclid(DAY_SECS);
    }
    local.tm_hour = 0;
    local.tm_min = 0;
    local.tm_sec = 0;
    local.tm_isdst = -1;
    let start = unsafe { libc::mktime(&mut local) };
    if start < 0 {
        epoch - epoch.rem_euclid(DAY_SECS)
    } else {
        start as i64
    }
}

#[cfg(not(unix))]
fn local_day_start(epoch: i64) -> i64 {
    epoch - epoch.rem_euclid(DAY_SECS)
}

fn calendar_starts(now: i64, days: usize) -> Vec<i64> {
    let mut newest_first = Vec::with_capacity(days);
    let mut start = local_day_start(now);
    for _ in 0..days {
        newest_first.push(start);
        start = local_day_start(start - 12 * 3_600);
    }
    newest_first.reverse();
    newest_first
}

fn aggregate_days(entries: &[LogEntry], starts: &[i64]) -> Vec<Totals> {
    let mut totals = vec![Totals::default(); starts.len()];
    let Some(first) = starts.first() else {
        return totals;
    };
    for entry in entries.iter().filter(|entry| entry.at >= *first) {
        let next = starts.partition_point(|start| *start <= entry.at);
        if next == 0 {
            continue;
        }
        let slot = &mut totals[next - 1];
        slot.tokens = slot.tokens.saturating_add(entry.tokens);
        if let Some(cost) = entry.cost {
            slot.cost += cost;
            slot.cost_rows = slot.cost_rows.saturating_add(1);
        } else {
            slot.missing_cost_rows = slot.missing_cost_rows.saturating_add(1);
        }
    }
    totals
}

fn append_history(provider: &mut Provider, history: &History, now: i64) {
    let has_history = history
        .daily
        .iter()
        .any(|total| total.tokens > 0 || total.cost_rows > 0);
    let mut trend = Metric::new(
        "Usage Trend",
        if has_history {
            String::new()
        } else {
            "No data".into()
        },
        Level::Ok,
    );
    if has_history {
        let has_tokens = history.daily.iter().any(|total| total.tokens > 0);
        trend.spark = history
            .daily
            .iter()
            .map(|total| {
                if has_tokens {
                    total.tokens as f64
                } else {
                    total.cost
                }
            })
            .collect();
    }
    provider.metrics.push(trend);

    let today = history.daily.last().copied().unwrap_or_default();
    let yesterday = history
        .daily
        .get(history.daily.len().saturating_sub(2))
        .copied()
        .unwrap_or_default();
    let last_30 = history
        .daily
        .iter()
        .fold(Totals::default(), |mut total, day| {
            total.cost += day.cost;
            total.cost_rows = total.cost_rows.saturating_add(day.cost_rows);
            total.missing_cost_rows = total
                .missing_cost_rows
                .saturating_add(day.missing_cost_rows);
            total.tokens = total.tokens.saturating_add(day.tokens);
            total
        });
    provider
        .metrics
        .push(Metric::new("Today", history_value(today), Level::Ok));
    provider.metrics.push(Metric::new(
        "Yesterday",
        history_value(yesterday),
        Level::Ok,
    ));
    provider.metrics.push(Metric::new(
        "Last 30 Days",
        history_value(last_30),
        Level::Ok,
    ));
    provider.day_usd = (today.cost_rows > 0).then_some(today.cost);
    if provider.status_fragment.is_none() {
        provider.status_fragment = if today.cost_rows > 0 {
            Some(format!("Grok {}", compact_usd(today.cost)))
        } else if today.tokens > 0 {
            Some(format!("Grok {} tok", compact_tokens(today.tokens)))
        } else {
            None
        };
    }

    let history_start = history
        .starts
        .first()
        .copied()
        .unwrap_or(now - LOOKBACK_SECS);
    let mut by_model: BTreeMap<&str, Totals> = BTreeMap::new();
    for entry in history
        .entries
        .iter()
        .filter(|entry| entry.at >= history_start)
    {
        let total = by_model.entry(entry.model.as_str()).or_default();
        total.tokens = total.tokens.saturating_add(entry.tokens);
        if let Some(cost) = entry.cost {
            total.cost += cost;
            total.cost_rows = total.cost_rows.saturating_add(1);
        } else {
            total.missing_cost_rows = total.missing_cost_rows.saturating_add(1);
        }
    }
    let mut models: Vec<(&str, Totals)> = by_model.into_iter().collect();
    models.sort_by(|left, right| {
        right
            .1
            .cost
            .total_cmp(&left.1.cost)
            .then_with(|| right.1.tokens.cmp(&left.1.tokens))
    });
    for (model, total) in models.into_iter().take(4) {
        provider.detail.push((
            model.strip_prefix("grok-").unwrap_or(model).to_string(),
            model_history_value(total),
        ));
    }
}

fn history_value(total: Totals) -> String {
    if total.tokens == 0 && total.cost_rows == 0 {
        return "No data".into();
    }
    if total.cost_rows == 0 {
        return format!("{} tokens", compact_tokens(total.tokens));
    }
    let incomplete = if total.missing_cost_rows > 0 { "+" } else { "" };
    format!(
        "{}{incomplete} · {} tokens",
        precise_usd(total.cost),
        compact_tokens(total.tokens)
    )
}

fn model_history_value(total: Totals) -> String {
    if total.cost_rows == 0 {
        return format!("{} tok", compact_tokens(total.tokens));
    }
    let incomplete = if total.missing_cost_rows > 0 { "+" } else { "" };
    format!(
        "{}{incomplete} · {} tok",
        precise_usd(total.cost),
        compact_tokens(total.tokens)
    )
}

fn precise_usd(amount: f64) -> String {
    if amount >= 1_000.0 {
        format!("${:.1}K", amount / 1_000.0)
    } else {
        format!("${amount:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_weekly_credits_and_disabled_extra_usage() {
        let body = r#"{"config":{"creditUsagePercent":99.0,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52.140114+00:00","end":"2026-07-07T21:36:52.140114+00:00"},"onDemandCap":{"val":0}}}"#;
        let billing = parse_billing(body).expect("valid captured billing shape");
        let weekly = billing.weekly.expect("weekly pool");
        assert_eq!(weekly.used, 99.0);
        assert_eq!(billing.extra_cap, 0.0);
        assert!(weekly.resets_at > weekly.starts_at);
    }

    #[test]
    fn absent_proto_zero_fields_decode_as_zero() {
        let body = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"}}}"#;
        let billing = parse_billing(body).expect("valid proto defaults");
        assert_eq!(billing.weekly.expect("weekly").used, 0.0);
        assert_eq!(billing.extra_cap, 0.0);

        let empty_cap = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-06-30T21:36:52Z","end":"2026-07-07T21:36:52Z"},"onDemandCap":{}}}"#;
        assert_eq!(
            parse_billing(empty_cap)
                .expect("empty proto message")
                .extra_cap,
            0.0
        );
    }

    #[test]
    fn monthly_period_is_not_mislabeled_as_weekly() {
        let body = r#"{"config":{"creditUsagePercent":42,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY","start":"2026-06-01T00:00:00Z","end":"2026-07-01T00:00:00Z"}}}"#;
        let billing = parse_billing(body).expect("valid monthly response");
        assert!(billing.weekly.is_none());
    }

    #[test]
    fn parses_completed_turn_without_double_counting_reasoning() {
        let line = r#"{"timestamp":"2026-06-10T10:00:00Z","method":"session/update","params":{"_meta":{"eventId":"turn-1"},"update":{"sessionUpdate":"turn_completed","usage":{"costUsdTicks":2357158800,"modelUsage":{"grok-4.6-build":{"inputTokens":1000000,"cachedReadTokens":700000,"outputTokens":50000,"reasoningTokens":20000,"costUsdTicks":2357158800}}}}}}"#;
        let value: Value = serde_json::from_str(line).unwrap();
        let rows = parse_completed_turn(&value);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens, 1_050_000);
        assert!((rows[0].cost.unwrap() - 0.23571588).abs() < 0.00000001);
        assert_eq!(rows[0].event_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn jwt_expiry_and_utc_format_round_trip() {
        // {"exp":1770000000}, base64url without padding.
        assert_eq!(
            jwt_expiry("header.eyJleHAiOjE3NzAwMDAwMDB9.signature"),
            Some(1_770_000_000)
        );
        assert_eq!(format_utc_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc_epoch(1_770_000_000), "2026-02-02T02:40:00Z");
    }

    #[test]
    fn refreshed_auth_preserves_other_accounts_and_unknown_fields() {
        let directory = temporary_directory("auth");
        let path = directory.join("auth.json");
        std::fs::write(
            &path,
            r#"{"issuer::client":{"key":"old","refresh_token":"old-refresh","custom":"keep"},"other":{"key":"untouched"}}"#,
        )
        .unwrap();
        persist_refreshed_auth(
            &path,
            "issuer::client",
            &RefreshResponse {
                access_token: "new".into(),
                refresh_token: Some("new-refresh".into()),
                id_token: None,
                expires_at: Some(1_770_000_000),
            },
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            value.pointer("/issuer::client/key").and_then(Value::as_str),
            Some("new")
        );
        assert_eq!(
            value
                .pointer("/issuer::client/custom")
                .and_then(Value::as_str),
            Some("keep")
        );
        assert_eq!(
            value.pointer("/other/key").and_then(Value::as_str),
            Some("untouched")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn partial_jsonl_line_waits_for_the_next_append() {
        let complete = br#"{"timestamp":"2026-06-10T10:00:00Z","update":{"sessionUpdate":"turn_completed","usage":{"modelUsage":{"grok-build":{"inputTokens":10}}}}}"#;
        let mut data = complete.to_vec();
        data.push(b'\n');
        data.extend_from_slice(&complete[..30]);
        let (entries, pending) = parse_complete_lines(&data);
        assert_eq!(entries.len(), 1);
        assert_eq!(pending, complete[..30]);
    }

    #[test]
    fn unchanged_session_logs_are_cached_and_appends_are_incremental() {
        use std::io::Write as _;

        let directory = temporary_directory("incremental");
        let path = directory.join("updates.jsonl");
        let first = r#"{"timestamp":"2026-06-10T10:00:00Z","params":{"_meta":{"eventId":"turn-1"},"update":{"sessionUpdate":"turn_completed","usage":{"modelUsage":{"grok-build":{"inputTokens":10}}}}}}"#;
        let second = r#"{"timestamp":"2026-06-10T11:00:00Z","params":{"_meta":{"eventId":"turn-2"},"update":{"sessionUpdate":"turn_completed","usage":{"modelUsage":{"grok-build":{"inputTokens":20}}}}}}"#;
        std::fs::write(&path, format!("{first}\n")).unwrap();

        assert_eq!(cached_log_entries(&path).len(), 1);
        assert_eq!(cached_log_entries(&path).len(), 1);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{second}").unwrap();
        drop(file);

        let rows = cached_log_entries(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().map(|row| row.tokens).collect::<Vec<_>>(),
            [10, 20]
        );

        log_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&path);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "unpeel-usage-grok-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
}
