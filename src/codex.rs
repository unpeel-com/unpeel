//! Codex CLI: the rollout logs under `~/.codex/sessions/` record a
//! `rate_limits` snapshot with every token count — real window utilization
//! percentages, reset times, and (on credit plans) the actual credits
//! balance. We read the newest snapshot from the most recent rollouts; no
//! estimation involved.

use crate::config::Config;
use crate::sources::{
    aggregate_monthly_tokens, aggregate_project_usage, compact_tokens, compact_usd,
    level_for_used_percent, normalize_project_path, read_tail, Level, Metric, MonthUsage,
    ProjectUsage, Provider, ProviderKind,
};
use crate::timeparse::{compact_duration, now_epoch_secs, parse_epoch_secs};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const TAIL_BYTES: u64 = 512 * 1024;
const NEWEST_FILES_TO_TRY: usize = 6;
/// A snapshot this old is shown but visibly dated.
const STALE_AFTER_SECS: i64 = 6 * 3_600;
const MONTH_HISTORY_SECS: i64 = 400 * 24 * 3_600;

fn sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".codex").join("sessions"))
}

struct RateSnapshot {
    line: Value,
    taken_at: Option<i64>,
    total_tokens: Option<u64>,
}

#[derive(Clone)]
struct CachedRolloutUsage {
    len: u64,
    modified: Option<std::time::SystemTime>,
    usage: Option<RolloutUsage>,
}

#[derive(Clone)]
struct RolloutUsage {
    at: i64,
    tokens: u64,
    project: Option<PathBuf>,
}

fn rollout_cache() -> &'static Mutex<HashMap<PathBuf, CachedRolloutUsage>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedRolloutUsage>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Newest rollout files first, without walking years of history: take the
/// lexicographically-last year/month/day directories (they are zero-padded
/// dates) and the files inside by mtime.
fn newest_rollouts(root: &PathBuf) -> Vec<PathBuf> {
    fn last_dirs(dir: &PathBuf, take: usize) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        dirs.into_iter().rev().take(take).collect()
    }
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for year in last_dirs(root, 1) {
        for month in last_dirs(&year, 2) {
            for day in last_dirs(&month, 4) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.filter_map(|entry| entry.ok()) {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "jsonl") {
                        if let Ok(meta) = entry.metadata() {
                            if let Ok(mtime) = meta.modified() {
                                files.push((mtime, path));
                            }
                        }
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().map(|(_, path)| path).collect()
}

/// Last `rate_limits`-bearing line in the newest rollouts, plus the last
/// total token count seen alongside it.
fn latest_snapshot(root: &PathBuf) -> Option<RateSnapshot> {
    for path in newest_rollouts(root).into_iter().take(NEWEST_FILES_TO_TRY) {
        let Some(tail) = read_tail(&path, TAIL_BYTES) else {
            continue;
        };
        let mut snapshot: Option<RateSnapshot> = None;
        let mut total_tokens: Option<u64> = None;
        for line in tail.lines() {
            if !line.contains("\"rate_limits\"") && !line.contains("\"token_count\"") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let payload = value.get("payload").unwrap_or(&value);
            if let Some(tokens) = payload
                .pointer("/info/total_token_usage/total_tokens")
                .and_then(Value::as_u64)
            {
                total_tokens = Some(tokens);
            }
            if payload.get("rate_limits").is_some() {
                snapshot = Some(RateSnapshot {
                    taken_at: value
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_epoch_secs),
                    total_tokens,
                    line: payload.clone(),
                });
            }
        }
        if snapshot.is_some() {
            return snapshot;
        }
    }
    None
}

/// One final cumulative token count per rollout. Codex writes many snapshots
/// during a session, so summing every `token_count` event would multiply the
/// same tokens; the last count in each rollout is the session total.
fn monthly_history(root: &Path, now: i64) -> (Vec<MonthUsage>, Vec<ProjectUsage>) {
    fn collect(dir: &std::path::Path, cutoff: std::time::SystemTime, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                collect(&path, cutoff, files);
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                && entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(|modified| modified >= cutoff)
            {
                files.push(path);
            }
        }
    }

    let cutoff = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(now.saturating_sub(MONTH_HISTORY_SECS).max(0) as u64);
    let mut files = Vec::new();
    collect(root, cutoff, &mut files);
    let visible = files
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    rollout_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|path, _| visible.contains(path));
    let entries: Vec<RolloutUsage> = files
        .iter()
        .filter_map(|path| cached_final_usage(path))
        .collect();
    (
        aggregate_monthly_tokens(entries.iter().map(|entry| (entry.at, entry.tokens))),
        aggregate_project_usage(
            entries
                .iter()
                .map(|entry| (entry.at, entry.tokens, entry.project.clone())),
        ),
    )
}

fn cached_final_usage(path: &std::path::Path) -> Option<RolloutUsage> {
    let metadata = std::fs::metadata(path).ok()?;
    let len = metadata.len();
    let modified = metadata.modified().ok();
    {
        let cache = rollout_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.get(path) {
            if cached.len == len && cached.modified == modified {
                return cached.usage.clone();
            }
        }
    }
    let tail = read_tail(path, TAIL_BYTES)?;
    let mut usage = None;
    for line in tail.lines().filter(|line| line.contains("\"token_count\"")) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&value);
        let Some(tokens) = payload
            .pointer("/info/total_token_usage/total_tokens")
            .and_then(Value::as_u64)
        else {
            continue;
        };
        let at = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_epoch_secs)
            .or_else(|| {
                modified?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs() as i64)
            })?;
        usage = Some(RolloutUsage {
            at,
            tokens,
            project: None,
        });
    }
    if let Some(usage) = usage.as_mut() {
        usage.project = rollout_project(path);
    }
    rollout_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            path.to_path_buf(),
            CachedRolloutUsage {
                len,
                modified,
                usage: usage.clone(),
            },
        );
    usage
}

fn rollout_project(path: &Path) -> Option<PathBuf> {
    use std::io::Read as _;

    let file = std::fs::File::open(path).ok()?;
    let mut head = String::new();
    file.take(256 * 1024).read_to_string(&mut head).ok()?;
    for line in head.lines() {
        if !line.contains("\"session_meta\"") || !line.contains("\"cwd\"") {
            continue;
        }
        let value = serde_json::from_str::<Value>(line).ok()?;
        let cwd = value.pointer("/payload/cwd").and_then(Value::as_str)?;
        if !cwd.trim().is_empty() {
            return Some(normalize_project_path(Path::new(cwd)));
        }
    }
    None
}

/// A window's label from its length: Codex uses 300 (5h) and 10080 (week).
fn window_label(minutes: Option<u64>) -> String {
    match minutes {
        Some(m) if m >= 10_080 => "week".into(),
        Some(m) if m >= 60 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
        None => "window".into(),
    }
}

pub fn scan(config: &Config) -> Provider {
    let mut provider = Provider {
        kind: ProviderKind::Codex,
        name: "Codex".into(),
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
    let Some(root) = sessions_root().filter(|root| root.is_dir()) else {
        return provider;
    };
    provider.present = true;
    let now = now_epoch_secs();
    (provider.monthly_tokens, provider.project_usage) = monthly_history(&root, now);
    let Some(snapshot) = latest_snapshot(&root) else {
        return provider;
    };
    let limits = &snapshot.line["rate_limits"];
    provider.as_of = snapshot.taken_at;
    if let Some(plan) = limits.get("plan_type").and_then(Value::as_str) {
        provider.badge = plan.to_string();
    }

    let stale = snapshot
        .taken_at
        .is_some_and(|taken| now - taken > STALE_AFTER_SECS);

    for key in ["primary", "secondary"] {
        let window = &limits[key];
        let Some(used) = window.get("used_percent").and_then(Value::as_f64) else {
            continue;
        };
        let label = window_label(window.get("window_minutes").and_then(Value::as_u64));
        // Newer Codex writes an absolute `resets_at`; older builds wrote
        // `resets_in_seconds` relative to the snapshot.
        let resets_secs = window
            .get("resets_at")
            .and_then(Value::as_i64)
            .map(|at| at - now)
            .or_else(|| {
                let relative = window.get("resets_in_seconds").and_then(Value::as_i64)?;
                Some(relative - (now - snapshot.taken_at.unwrap_or(now)))
            });
        let resets = resets_secs
            .filter(|secs| *secs > 0)
            .map(|secs| format!(" · resets {}", compact_duration(secs)))
            .unwrap_or_default();
        let level = level_for_used_percent(used, config.alerts.codex_used_percent);
        if level == Level::Alert && provider.alert.is_none() {
            provider.alert = Some(format!("Codex {label} at {used:.0}%"));
        }
        if provider.status_fragment.is_none() {
            provider.status_fragment = Some(format!("Codex {used:.0}%"));
        }
        let mut metric = Metric::new(label, format!("{used:.0}%{resets}"), level);
        metric.percent = Some(used.clamp(0.0, 100.0));
        provider.metrics.push(metric);
    }

    let credits = &limits["credits"];
    if credits.get("has_credits").and_then(Value::as_bool) == Some(true)
        && credits.get("unlimited").and_then(Value::as_bool) != Some(true)
    {
        if let Some(balance) = credits
            .get("balance")
            .and_then(Value::as_str)
            .and_then(|raw| raw.parse::<f64>().ok())
        {
            let low = config.alerts.credits_low_usd;
            let level = if low > 0.0 && balance <= low {
                Level::Alert
            } else if low > 0.0 && balance <= low * 2.0 {
                Level::Warn
            } else {
                Level::Ok
            };
            if level == Level::Alert {
                provider.alert = Some(format!("Codex credits low: {}", compact_usd(balance)));
            }
            provider
                .metrics
                .push(Metric::new("credits", compact_usd(balance), level));
        }
    }

    if let Some(tokens) = snapshot.total_tokens {
        provider
            .detail
            .push(("session tokens".into(), compact_tokens(tokens)));
    }
    if let Some(taken) = snapshot.taken_at {
        provider.detail.push((
            "snapshot".into(),
            format!("{} ago", compact_duration(now - taken)),
        ));
    }
    if stale {
        provider.detail.push((
            "note".into(),
            "no recent Codex activity; numbers are from the last run".into(),
        ));
    }
    provider
}
