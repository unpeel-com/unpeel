//! Muse Code usage from its durable local event log. Muse mirrors some model
//! calls into parent and subagent streams, so `usage_id` is the deduplication
//! key. The model catalog supplies display metadata and prices; credentials
//! are never read into the provider model or shown in the UI.

use crate::sources::{
    aggregate_monthly_tokens, aggregate_project_usage, compact_tokens, compact_usd, Level, Metric,
    Provider, ProviderKind,
};
use crate::timeparse::now_epoch_secs;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_SECS: i64 = 24 * 3_600;
const HISTORY_DAYS: usize = 30;
const MONTH_HISTORY_SECS: i64 = 400 * DAY_SECS;
const MAXIMUM_PLAUSIBLE_TOKENS: u64 = 1_000_000_000_000;

#[derive(Clone, Copy, Debug, Default)]
struct Rates {
    input: f64,
    output: f64,
    cached: f64,
}

#[derive(Clone, Debug, Default)]
struct ModelInfo {
    label: String,
    rates: Option<Rates>,
}

#[derive(Clone, Debug)]
struct Entry {
    usage_id: Option<String>,
    at: i64,
    input: u64,
    output: u64,
    cached: u64,
    project: Option<PathBuf>,
}

impl Entry {
    fn tokens(&self) -> u64 {
        // Muse reports cached tokens as a subset of input tokens, not an
        // additional quantity. Reasoning tokens are likewise part of output.
        self.input.saturating_add(self.output)
    }

    fn cost(&self, rates: Option<Rates>) -> Option<f64> {
        let rates = rates?;
        let cached = self.cached.min(self.input);
        let uncached = self.input.saturating_sub(cached);
        Some(
            (uncached as f64 * rates.input
                + cached as f64 * rates.cached
                + self.output as f64 * rates.output)
                / 1_000_000.0,
        )
    }
}

#[derive(Clone)]
struct CachedLog {
    len: u64,
    modified: Option<SystemTime>,
    entries: Vec<Entry>,
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
    tokens: u64,
}

pub fn scan() -> Provider {
    let mut provider = Provider {
        kind: ProviderKind::Muse,
        name: "Muse".into(),
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
    let Some(home) = muse_data_home() else {
        return provider;
    };
    if !home.is_dir() {
        return provider;
    }
    provider.present = true;

    let now = now_epoch_secs();
    let model = read_model_info(&home);
    provider.badge = model_badge(&model.label);
    let entries = scan_entries(&home.join("sessions"), now);
    provider.as_of = entries.last().map(|entry| entry.at);
    provider.monthly_tokens =
        aggregate_monthly_tokens(entries.iter().map(|entry| (entry.at, entry.tokens())));
    provider.project_usage = aggregate_project_usage(
        entries
            .iter()
            .map(|entry| (entry.at, entry.tokens(), entry.project.clone())),
    );
    append_history(&mut provider, &entries, &model, now);
    provider
}

fn muse_data_home() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MUSE_DATA_DIR").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("muse"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/share/muse"))
}

fn read_model_info(home: &Path) -> ModelInfo {
    let directory = home.join("model-catalog");
    let Ok(files) = std::fs::read_dir(directory) else {
        return ModelInfo::default();
    };
    let mut candidates: Vec<PathBuf> = files
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    candidates.sort();
    for path in candidates.into_iter().rev() {
        let Ok(raw) = std::fs::read(&path) else {
            continue;
        };
        let Ok(catalog) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        if catalog.get("provider_id").and_then(Value::as_str) != Some("meta") {
            continue;
        }
        let Some(rows) = catalog.get("rows").and_then(Value::as_array) else {
            continue;
        };
        let row = rows
            .iter()
            .find(|row| row.get("is_default").and_then(Value::as_bool) == Some(true))
            .or_else(|| {
                rows.iter()
                    .find(|row| row.get("is_current").and_then(Value::as_bool) == Some(true))
            })
            .or_else(|| rows.first());
        let Some(row) = row else {
            continue;
        };
        let label = row
            .get("display_label")
            .or_else(|| row.get("model_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let rates = row.get("cost").and_then(parse_rates);
        return ModelInfo { label, rates };
    }
    ModelInfo::default()
}

fn parse_rates(cost: &Value) -> Option<Rates> {
    let number = |key: &str| {
        cost.get(key)
            .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
            .filter(|value| value.is_finite() && *value >= 0.0)
    };
    Some(Rates {
        input: number("input")?,
        output: number("output")?,
        cached: number("cached").unwrap_or_else(|| number("input").unwrap_or(0.0)),
    })
}

fn model_badge(label: &str) -> String {
    let label = label.strip_prefix("muse-").unwrap_or(label);
    let mut words = label.split('-');
    let Some(first) = words.next().filter(|word| !word.is_empty()) else {
        return String::new();
    };
    let mut first_chars = first.chars();
    let first = first_chars
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .chain(first_chars)
        .collect::<String>();
    std::iter::once(first)
        .chain(words.map(str::to_string))
        .collect::<Vec<_>>()
        .join(" ")
}

fn scan_entries(root: &Path, now: i64) -> Vec<Entry> {
    let since = now.saturating_sub(MONTH_HISTORY_SECS);
    let mut files = Vec::new();
    collect_session_logs(root, since, &mut files);
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
            .usage_id
            .as_ref()
            .is_none_or(|usage_id| seen.insert(usage_id.clone()))
    });
    entries.retain(|entry| entry.at >= since);
    entries.sort_by_key(|entry| entry.at);
    entries
}

fn collect_session_logs(directory: &Path, since: i64, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_session_logs(&path, since, output);
            continue;
        }
        if !file_type.is_file()
            || path.file_name().and_then(|name| name.to_str()) != Some("session.jsonl")
        {
            continue;
        }
        let recent_enough = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .is_none_or(|modified| modified.as_secs() as i64 >= since);
        if recent_enough {
            output.push(path);
        }
    }
}

fn cached_log_entries(path: &Path) -> Vec<Entry> {
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
    let file = std::fs::File::open(path).ok()?;
    let mut head = String::new();
    file.take(256 * 1024).read_to_string(&mut head).ok()?;
    for line in head.lines() {
        if !line.contains("workspace_root") {
            continue;
        }
        let value = serde_json::from_str::<Value>(line).ok()?;
        let root = value
            .pointer("/payload/record/workspace_root")
            .and_then(Value::as_str)?;
        if !root.trim().is_empty() {
            return Some(PathBuf::from(root));
        }
    }
    None
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

fn parse_complete_lines(data: &[u8]) -> (Vec<Entry>, Vec<u8>) {
    let complete_end = data
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let entries = data[..complete_end]
        .split(|byte| *byte == b'\n')
        .filter(|line| {
            line.windows(b"goal_usage_attribution".len())
                .any(|window| window == b"goal_usage_attribution")
        })
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter_map(|value| parse_usage_record(&value))
        .collect();
    (entries, data[complete_end..].to_vec())
}

fn parse_usage_record(value: &Value) -> Option<Entry> {
    let event = value.pointer("/payload/event")?;
    if event.get("kind").and_then(Value::as_str) != Some("goal_usage_attribution") {
        return None;
    }
    let record = event.get("record")?;
    if record.get("usage_family").and_then(Value::as_str) != Some("provider") {
        return None;
    }
    let quantity = record.get("quantity")?;
    if quantity.get("unit").and_then(Value::as_str) != Some("tokens")
        || quantity.get("reported").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let input = bounded_tokens(quantity.get("input_tokens")?)?;
    let output = bounded_tokens(quantity.get("output_tokens")?)?;
    let cached = quantity
        .get("cached_tokens")
        .and_then(bounded_tokens)
        .unwrap_or(0);
    let raw_at = value.get("recorded_at")?.as_i64()?;
    let at = normalize_timestamp(raw_at)?;
    Some(Entry {
        usage_id: record
            .get("usage_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        at,
        input,
        output,
        cached,
        project: None,
    })
}

fn bounded_tokens(value: &Value) -> Option<u64> {
    let number = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))?;
    (number <= MAXIMUM_PLAUSIBLE_TOKENS).then_some(number)
}

fn normalize_timestamp(raw: i64) -> Option<i64> {
    if raw <= 0 {
        return None;
    }
    Some(if raw >= 10_000_000_000_000 {
        raw / 1_000_000
    } else if raw >= 10_000_000_000 {
        raw / 1_000
    } else {
        raw
    })
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

fn aggregate_days(entries: &[Entry], starts: &[i64], rates: Option<Rates>) -> Vec<Totals> {
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
        slot.tokens = slot.tokens.saturating_add(entry.tokens());
        if let Some(cost) = entry.cost(rates) {
            slot.cost += cost;
            slot.cost_rows = slot.cost_rows.saturating_add(1);
        }
    }
    totals
}

fn append_history(provider: &mut Provider, entries: &[Entry], model: &ModelInfo, now: i64) {
    let starts = calendar_starts(now, HISTORY_DAYS);
    let daily = aggregate_days(entries, &starts, model.rates);
    let has_history = daily.iter().any(|total| total.tokens > 0);
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
        trend.spark = daily.iter().map(|total| total.tokens as f64).collect();
    }
    provider.metrics.push(trend);

    let today = daily.last().copied().unwrap_or_default();
    let yesterday = daily
        .get(daily.len().saturating_sub(2))
        .copied()
        .unwrap_or_default();
    let last_30 = daily.iter().fold(Totals::default(), |mut sum, total| {
        sum.cost += total.cost;
        sum.cost_rows = sum.cost_rows.saturating_add(total.cost_rows);
        sum.tokens = sum.tokens.saturating_add(total.tokens);
        sum
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
    provider.status_fragment = if today.cost_rows > 0 {
        Some(format!("Muse {}", compact_usd(today.cost)))
    } else if today.tokens > 0 {
        Some(format!("Muse {} tok", compact_tokens(today.tokens)))
    } else {
        None
    };
    if last_30.tokens > 0 && !model.label.is_empty() {
        provider
            .detail
            .push((model_badge(&model.label), model_history_value(last_30)));
    }
}

fn history_value(total: Totals) -> String {
    if total.tokens == 0 {
        return "No data".into();
    }
    if total.cost_rows == 0 {
        return format!("{} tokens", compact_tokens(total.tokens));
    }
    format!(
        "{} est · {} tokens",
        precise_usd(total.cost),
        compact_tokens(total.tokens)
    )
}

fn model_history_value(total: Totals) -> String {
    if total.cost_rows == 0 {
        return format!("{} tok", compact_tokens(total.tokens));
    }
    format!(
        "{} est · {} tok",
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
    use serde_json::json;

    #[test]
    fn parses_reported_provider_usage_and_normalizes_microseconds() {
        let value = json!({
            "recorded_at": 1_787_323_041_632_458_i64,
            "payload": {"event": {
                "kind": "goal_usage_attribution",
                "record": {
                    "usage_id": "usage-one",
                    "usage_family": "provider",
                    "quantity": {
                        "unit": "tokens",
                        "reported": true,
                        "input_tokens": 27_330,
                        "output_tokens": 1_355,
                        "cached_tokens": 25_905,
                        "reasoning_tokens": 1_039
                    }
                }
            }}
        });
        let entry = parse_usage_record(&value).expect("provider attribution");
        assert_eq!(entry.at, 1_787_323_041);
        assert_eq!(entry.tokens(), 28_685);
        assert_eq!(entry.cached, 25_905);
    }

    #[test]
    fn ignores_non_provider_usage_attribution() {
        let value = json!({
            "recorded_at": 1_787_323_041_632_458_i64,
            "payload": {"event": {
                "kind": "goal_usage_attribution",
                "record": {
                    "usage_family": "tool",
                    "quantity": {"unit": "tokens", "reported": false,
                        "input_tokens": 0, "output_tokens": 0}
                }
            }}
        });
        assert!(parse_usage_record(&value).is_none());
    }

    #[test]
    fn cached_input_uses_the_catalogs_discounted_rate() {
        let entry = Entry {
            usage_id: None,
            at: 1,
            input: 1_000_000,
            output: 1_000_000,
            cached: 750_000,
            project: None,
        };
        let cost = entry
            .cost(Some(Rates {
                input: 1.25,
                output: 4.25,
                cached: 0.15,
            }))
            .expect("known rates");
        assert!((cost - 4.675).abs() < 0.000_001);
    }

    #[test]
    fn formats_current_muse_model_as_a_short_badge() {
        assert_eq!(model_badge("muse-spark-1.2"), "Spark 1.2");
    }
}
