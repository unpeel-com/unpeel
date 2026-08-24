//! Claude Code: the conversation transcripts under `~/.claude/projects/`
//! carry per-message token usage. Claude records no quota locally, so this
//! source reports estimated spend — the rolling 24h total and the current
//! 5-hour billing block (Anthropic's limit windows start at the first
//! message and last five hours, anchored to the hour). Costs use public
//! per-model API prices and are labeled as estimates.

use crate::config::Config;
use crate::sources::{compact_tokens, compact_usd, read_tail, Level, Metric, Provider};
use crate::timeparse::{compact_duration, now_epoch_secs, parse_epoch_secs};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Only transcripts touched within the window (plus block slack) matter.
const LOOKBACK_SECS: i64 = 26 * 3_600;
const BLOCK_SECS: i64 = 5 * 3_600;
const TAIL_BYTES: u64 = 4 * 1024 * 1024;

fn projects_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".claude").join("projects"))
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

pub fn scan(config: &Config) -> Provider {
    let mut provider = Provider {
        name: "Claude Code",
        badge: String::new(),
        present: false,
        metrics: Vec::new(),
        detail: Vec::new(),
        as_of: None,
        alert: None,
        status_fragment: None,
    };
    let Some(root) = projects_root().filter(|root| root.is_dir()) else {
        return provider;
    };
    provider.present = true;
    let now = now_epoch_secs();
    let entries = recent_entries(&root, now);
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
            "Claude block at {} (budget {})",
            compact_usd(block_cost),
            compact_usd(budget)
        ));
    }
    let resets = block
        .map(|start| format!(" · resets {}", compact_duration(start + BLOCK_SECS - now)))
        .unwrap_or_default();
    provider.metrics.push(Metric {
        label: "5h block".into(),
        percent: (budget > 0.0).then(|| (block_cost / budget * 100.0).clamp(0.0, 100.0)),
        value: format!("{} est{resets}", compact_usd(block_cost)),
        level: block_level,
    });

    let day_cost: f64 = entries.iter().map(|entry| entry.cost).sum();
    let day_tokens: u64 = entries.iter().map(|entry| entry.tokens).sum();
    provider.metrics.push(Metric {
        label: "24h".into(),
        percent: None,
        value: format!(
            "{} est · {} tok",
            compact_usd(day_cost),
            compact_tokens(day_tokens)
        ),
        level: Level::Ok,
    });

    provider.status_fragment = Some(format!("Claude {}", compact_usd(block_cost)));

    // Per-model 24h breakdown for the expanded card, largest first.
    let mut by_model: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
    for entry in &entries {
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
    provider
}
