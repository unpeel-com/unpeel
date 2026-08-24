//! Codex CLI: the rollout logs under `~/.codex/sessions/` record a
//! `rate_limits` snapshot with every token count — real window utilization
//! percentages, reset times, and (on credit plans) the actual credits
//! balance. We read the newest snapshot from the most recent rollouts; no
//! estimation involved.

use crate::config::Config;
use crate::sources::{
    compact_tokens, compact_usd, level_for_used_percent, read_tail, Level, Metric, Provider,
};
use crate::timeparse::{compact_duration, now_epoch_secs, parse_epoch_secs};
use serde_json::Value;
use std::path::PathBuf;

const TAIL_BYTES: u64 = 512 * 1024;
const NEWEST_FILES_TO_TRY: usize = 6;
/// A snapshot this old is shown but visibly dated.
const STALE_AFTER_SECS: i64 = 6 * 3_600;

fn sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".codex").join("sessions"))
}

struct RateSnapshot {
    line: Value,
    taken_at: Option<i64>,
    total_tokens: Option<u64>,
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
        name: "Codex",
        badge: String::new(),
        present: false,
        metrics: Vec::new(),
        detail: Vec::new(),
        as_of: None,
        alert: None,
        status_fragment: None,
    };
    let Some(root) = sessions_root().filter(|root| root.is_dir()) else {
        return provider;
    };
    provider.present = true;
    let Some(snapshot) = latest_snapshot(&root) else {
        return provider;
    };
    let now = now_epoch_secs();
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
        provider.metrics.push(Metric {
            label,
            percent: Some(used.clamp(0.0, 100.0)),
            value: format!("{used:.0}%{resets}"),
            level,
        });
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
            provider.metrics.push(Metric {
                label: "credits".into(),
                percent: None,
                value: compact_usd(balance),
                level,
            });
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
