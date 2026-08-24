//! Claude Code: the conversation transcripts under `<config dir>/projects/`
//! carry per-message token usage. Claude records no quota locally, so this
//! source reports estimated spend — the rolling 24h total and the current
//! 5-hour billing block (Anthropic's limit windows start at the first
//! message and last five hours, anchored to the hour). Costs use public
//! per-model API prices and are labeled as estimates.
//!
//! Multiple accounts: people run second accounts by pointing
//! `CLAUDE_CONFIG_DIR` at an alternate directory. Every such directory that
//! holds transcripts becomes its own card — the default `~/.claude` (or the
//! current `CLAUDE_CONFIG_DIR`), any `~/.claude-*` sibling, and dirs listed
//! under `[claude] dirs` in config.toml. The account email in the dir's
//! `.claude.json` labels the card.

use crate::config::Config;
use crate::sources::{
    compact_tokens, compact_usd, read_tail, Level, Metric, Provider, ProviderKind,
};
use crate::timeparse::{compact_duration, now_epoch_secs, parse_epoch_secs};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Only transcripts touched within the window (plus block slack) matter.
const LOOKBACK_SECS: i64 = 26 * 3_600;
const DAY_SECS: i64 = 24 * 3_600;
const BLOCK_SECS: i64 = 5 * 3_600;
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// One sparkline cell per hour of the trailing day.
const SPARK_BUCKETS: usize = 24;
/// Burn rate needs this much of a block elapsed to mean anything.
const BURN_MIN_ELAPSED_SECS: i64 = 15 * 60;

/// One Claude Code config directory — one account.
struct Account {
    dir: PathBuf,
    /// Dir-derived suffix: `~/.claude` → None, `~/.claude-work` → "work".
    short: Option<String>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn expand_home(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/").and_then(|rest| Some(home()?.join(rest))) {
        Some(path) => path,
        None => PathBuf::from(raw),
    }
}

fn short_label(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_string_lossy().to_string();
    let trimmed = name
        .trim_start_matches('.')
        .trim_start_matches("claude")
        .trim_start_matches(['-', '_']);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Every Claude config dir on this machine that holds transcripts, default
/// account first, deduped by canonical path.
fn discover_accounts(config: &Config) -> Vec<Account> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
        dirs.push(dir);
    }
    if let Some(home) = home() {
        dirs.push(home.join(".claude"));
        if let Ok(entries) = std::fs::read_dir(&home) {
            let mut siblings: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with(".claude-") || name.starts_with(".claude_")
                        })
                })
                .collect();
            siblings.sort();
            dirs.extend(siblings);
        }
    }
    dirs.extend(config.claude.dirs.iter().map(|raw| expand_home(raw)));

    let mut seen: HashSet<PathBuf> = HashSet::new();
    dirs.into_iter()
        .filter(|dir| dir.join("projects").is_dir())
        .filter(|dir| seen.insert(dir.canonicalize().unwrap_or_else(|_| dir.clone())))
        .map(|dir| Account {
            short: short_label(&dir),
            dir,
        })
        .collect()
}

/// The logged-in account's email, from the dir's `.claude.json` (the default
/// account keeps that file in `$HOME` instead).
fn account_email(dir: &Path) -> Option<String> {
    let mut candidates = vec![dir.join(".claude.json")];
    if let Some(home) = home() {
        if dir == home.join(".claude").as_path() {
            candidates.push(home.join(".claude.json"));
        }
    }
    for path in candidates {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(email) = value
            .pointer("/oauthAccount/emailAddress")
            .and_then(Value::as_str)
        {
            return Some(email.to_string());
        }
    }
    None
}

struct Entry {
    at: i64,
    model: String,
    cost: f64,
    tokens: u64,
}

/// Public API prices per million tokens (input, output). Cache reads bill at
/// 10% of input and cache writes at 125%; unknown models fall back to the
/// mid tier. This is deliberately a small table — the display says "est".
fn rates(model: &str) -> (f64, f64) {
    if model.contains("opus") {
        (15.0, 75.0)
    } else if model.contains("haiku") {
        (1.0, 5.0)
    } else {
        // sonnet and everything unrecognized
        (3.0, 15.0)
    }
}

fn message_cost(model: &str, usage: &Value) -> (f64, u64) {
    let tokens = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (input_rate, output_rate) = rates(model);
    let input = tokens("input_tokens");
    let output = tokens("output_tokens");
    let cache_read = tokens("cache_read_input_tokens");
    let cache_write = tokens("cache_creation_input_tokens");
    let cost = (input as f64 * input_rate
        + cache_read as f64 * input_rate * 0.1
        + cache_write as f64 * input_rate * 1.25
        + output as f64 * output_rate)
        / 1_000_000.0;
    (cost, input + output + cache_read + cache_write)
}

fn recent_entries(root: &PathBuf, now: i64) -> Vec<Entry> {
    let cutoff = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_secs((now - LOOKBACK_SECS).max(0) as u64);
    let mut entries = Vec::new();
    // A message can appear in several transcript files when a conversation
    // continues across sessions; the message id dedupes it.
    let mut seen_ids: HashSet<String> = HashSet::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return entries;
    };
    for project in projects.filter_map(|entry| entry.ok()) {
        let Ok(transcripts) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for transcript in transcripts.filter_map(|entry| entry.ok()) {
            let path = transcript.path();
            if !path.extension().is_some_and(|ext| ext == "jsonl") {
                continue;
            }
            let fresh = transcript
                .metadata()
                .and_then(|meta| meta.modified())
                .map(|mtime| mtime >= cutoff)
                .unwrap_or(false);
            if !fresh {
                continue;
            }
            let Some(tail) = read_tail(&path, TAIL_BYTES) else {
                continue;
            };
            for line in tail.lines() {
                if !line.contains("\"usage\"") {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if value.get("type").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                let Some(at) = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_epoch_secs)
                else {
                    continue;
                };
                if at < now - LOOKBACK_SECS {
                    continue;
                }
                let message = &value["message"];
                let Some(usage) = message.get("usage") else {
                    continue;
                };
                if let Some(id) = message.get("id").and_then(Value::as_str) {
                    if !seen_ids.insert(id.to_string()) {
                        continue;
                    }
                }
                let model = message
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let (cost, tokens) = message_cost(&model, usage);
                entries.push(Entry {
                    at,
                    model,
                    cost,
                    tokens,
                });
            }
        }
    }
    entries.sort_by_key(|entry| entry.at);
    entries
}

/// The current 5h block start: walk activity in order, opening a new block
/// (anchored to the hour) whenever a message lands after the previous block
/// ended. Mirrors how Anthropic's limit windows behave.
fn current_block_start(entries: &[Entry], now: i64) -> Option<i64> {
    let mut block_start: Option<i64> = None;
    for entry in entries {
        match block_start {
            Some(start) if entry.at < start + BLOCK_SECS => {}
            _ => block_start = Some(entry.at - entry.at % 3_600),
        }
    }
    block_start.filter(|start| now < start + BLOCK_SECS)
}

/// Hourly cost buckets over the trailing 24h, oldest first.
fn spark_buckets(entries: &[Entry], now: i64) -> Vec<f64> {
    let start = now - DAY_SECS;
    let mut buckets = vec![0.0; SPARK_BUCKETS];
    for entry in entries.iter().filter(|entry| entry.at >= start) {
        let index = ((entry.at - start) as usize * SPARK_BUCKETS / DAY_SECS as usize)
            .min(SPARK_BUCKETS - 1);
        buckets[index] += entry.cost;
    }
    buckets
}

/// One card per Claude account found on this machine. A machine with no
/// Claude at all still gets a single "not installed" card.
pub fn scan_all(config: &Config) -> Vec<Provider> {
    let accounts = discover_accounts(config);
    if accounts.is_empty() {
        return vec![Provider {
            kind: ProviderKind::Claude,
            name: "Claude Code".into(),
            badge: String::new(),
            present: false,
            metrics: Vec::new(),
            detail: Vec::new(),
            as_of: None,
            alert: None,
            status_fragment: None,
            day_usd: None,
        }];
    }
    let multi = accounts.len() > 1;
    accounts
        .iter()
        .map(|account| scan_account(config, account, multi))
        .collect()
}

fn scan_account(config: &Config, account: &Account, multi: bool) -> Provider {
    let email = account_email(&account.dir);
    let name = match (&account.short, multi) {
        (Some(short), true) => format!("Claude Code · {short}"),
        _ => "Claude Code".to_string(),
    };
    // The sidebar fragment names the account only when there is more than one.
    let fragment_name = match (&account.short, multi) {
        (Some(short), true) => short.clone(),
        _ => "Claude".to_string(),
    };
    let mut provider = Provider {
        kind: ProviderKind::Claude,
        name,
        badge: email.clone().unwrap_or_default(),
        present: true,
        metrics: Vec::new(),
        detail: Vec::new(),
        as_of: None,
        alert: None,
        status_fragment: None,
        day_usd: None,
    };
    let now = now_epoch_secs();
    let entries = recent_entries(&account.dir.join("projects"), now);
    if entries.is_empty() {
        return provider;
    }
    provider.as_of = entries.last().map(|entry| entry.at);

    let block = current_block_start(&entries, now);
    let block_cost: f64 = block
        .map(|start| {
            entries
                .iter()
                .filter(|entry| entry.at >= start)
                .map(|entry| entry.cost)
                .sum()
        })
        .unwrap_or(0.0);
    let budget = config.alerts.claude_block_usd;
    let block_level = if budget > 0.0 && block_cost >= budget {
        Level::Alert
    } else if budget > 0.0 && block_cost >= budget * 0.8 {
        Level::Warn
    } else {
        Level::Ok
    };
    if block_level == Level::Alert {
        provider.alert = Some(format!(
            "{fragment_name} block at {} (budget {})",
            compact_usd(block_cost),
            compact_usd(budget)
        ));
    }
    let resets = block
        .map(|start| format!(" · resets {}", compact_duration(start + BLOCK_SECS - now)))
        .unwrap_or_default();
    let mut block_metric = Metric::new(
        "5h block",
        format!("{} est{resets}", compact_usd(block_cost)),
        block_level,
    );
    block_metric.percent =
        (budget > 0.0).then(|| (block_cost / budget * 100.0).clamp(0.0, 100.0));
    provider.metrics.push(block_metric);

    // Burn rate over the active block, once enough of it has elapsed.
    if let Some(start) = block {
        let elapsed = now - start;
        if elapsed >= BURN_MIN_ELAPSED_SECS && block_cost > 0.0 {
            let per_hour = block_cost / (elapsed as f64 / 3_600.0);
            provider.metrics.push(Metric::new(
                "burn",
                format!("{}/hr est", compact_usd(per_hour)),
                Level::Ok,
            ));
        }
    }

    let day_entries: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.at >= now - DAY_SECS)
        .collect();
    let day_cost: f64 = day_entries.iter().map(|entry| entry.cost).sum();
    let day_tokens: u64 = day_entries.iter().map(|entry| entry.tokens).sum();
    let mut day_metric = Metric::new(
        "24h",
        format!(
            "{} est · {} tok",
            compact_usd(day_cost),
            compact_tokens(day_tokens)
        ),
        Level::Ok,
    );
    day_metric.spark = spark_buckets(&entries, now);
    provider.metrics.push(day_metric);
    provider.day_usd = Some(day_cost);

    provider.status_fragment = Some(format!("{fragment_name} {}", compact_usd(block_cost)));

    // Per-model 24h breakdown for the expanded card, largest first.
    let mut by_model: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
    for entry in &day_entries {
        let slot = by_model.entry(entry.model.as_str()).or_default();
        slot.0 += entry.cost;
        slot.1 += entry.tokens;
    }
    let mut models: Vec<(&str, (f64, u64))> = by_model.into_iter().collect();
    models.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0));
    for (model, (cost, tokens)) in models.into_iter().take(4) {
        let short = model.strip_prefix("claude-").unwrap_or(model);
        provider.detail.push((
            short.to_string(),
            format!("{} · {} tok", compact_usd(cost), compact_tokens(tokens)),
        ));
    }
    if multi {
        provider
            .detail
            .push(("account".into(), account.dir.to_string_lossy().into_owned()));
    }
    provider
}
