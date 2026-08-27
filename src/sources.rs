//! Provider model and scan orchestration. Codex and spend history come from
//! local tool files; Claude can additionally reuse Claude Code's OAuth login
//! for live subscription limits. No pasted API keys and no daemon.

use crate::config::Config;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Alert,
}

/// Whether a bounded row visualizes the amount consumed or the amount left.
/// Claude's live API reports utilization and OpenUsage presents it as used;
/// Codex's existing cards keep their remaining-first presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PercentDisplay {
    Remaining,
    Used,
}

/// Which tool a card describes — the UI keys its accent color off this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Codex,
    Claude,
}

/// One gauge/value row inside a provider card.
#[derive(Debug, Clone)]
pub struct Metric {
    pub label: String,
    /// 0..=100 renders a bar; None renders the value only.
    pub percent: Option<f64>,
    /// Hourly activity buckets, oldest first; non-empty renders a sparkline
    /// in place of the bar.
    pub spark: Vec<f64>,
    pub value: String,
    pub level: Level,
    pub percent_display: PercentDisplay,
    /// Optional pace projection shown opposite the label (for example
    /// `~4% spare` or `🔥 Limit in 56m`).
    pub annotation: Option<String>,
    /// Optional 0..=1 even-pace marker drawn over the meter.
    pub marker: Option<f64>,
}

impl Metric {
    pub fn new(label: impl Into<String>, value: String, level: Level) -> Self {
        Self {
            label: label.into(),
            percent: None,
            spark: Vec::new(),
            value,
            level,
            percent_display: PercentDisplay::Remaining,
            annotation: None,
            marker: None,
        }
    }

    pub fn used_percent(label: impl Into<String>, used: f64, value: String, level: Level) -> Self {
        let mut metric = Self::new(label, value, level);
        metric.percent = Some(used.clamp(0.0, 100.0));
        metric.percent_display = PercentDisplay::Used;
        metric
    }
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub kind: ProviderKind,
    pub name: String,
    pub badge: String,
    /// The tool's local data directory exists at all.
    pub present: bool,
    pub metrics: Vec<Metric>,
    /// Key/value rows revealed by Enter.
    pub detail: Vec<(String, String)>,
    /// Epoch seconds of the newest evidence backing the numbers.
    pub as_of: Option<i64>,
    /// One short alert sentence when any metric crossed its threshold.
    pub alert: Option<String>,
    /// Shortest fragment for the sidebar status line, e.g. "Codex 3%".
    pub status_fragment: Option<String>,
    /// Estimated spend over the trailing 24h, when the source can price it.
    pub day_usd: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub providers: Vec<Provider>,
}

impl Snapshot {
    pub fn scan(config: &Config) -> Self {
        let mut providers = Vec::new();
        for kind in provider_order() {
            match kind {
                ProviderKind::Codex => providers.push(crate::codex::scan(config)),
                ProviderKind::Claude => providers.extend(crate::claude::scan_all(config)),
            }
        }
        Self { providers }
    }

    /// Compact per-provider sidebar status. Notification event copy is
    /// transient and is written separately when an enabled alert edge fires.
    pub fn status_line(&self) -> String {
        let fragments: Vec<&str> = self
            .providers
            .iter()
            .filter_map(|provider| provider.status_fragment.as_deref())
            .collect();
        if fragments.is_empty() {
            "no local usage data".into()
        } else {
            fragments.join(" · ")
        }
    }
}

/// Use Unpeel's flat preset list as the provider catalog when an Unpeel home
/// exists and has readable preset state. Otherwise retain the standalone
/// dashboard's complete, stable order.
fn provider_order() -> Vec<ProviderKind> {
    unpeel_home()
        .as_deref()
        .and_then(unpeel_preset_provider_order)
        .unwrap_or_else(|| vec![ProviderKind::Codex, ProviderKind::Claude])
}

fn unpeel_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("UNPEEL_HOME").filter(|home| !home.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".unpeel"))
}

/// `Some` means Unpeel has an authoritative presets array, including an empty
/// one. `None` means there is no usable Unpeel preset contract, so callers
/// should keep standalone behavior.
fn unpeel_preset_provider_order(home: &Path) -> Option<Vec<ProviderKind>> {
    if !home.is_dir() {
        return None;
    }
    let raw = std::fs::read(home.join("app-state.json")).ok()?;
    provider_order_from_app_state(&raw)
}

fn provider_order_from_app_state(raw: &[u8]) -> Option<Vec<ProviderKind>> {
    let state: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let presets = state.get("presets")?.as_array()?;
    let mut order = Vec::new();
    for preset in presets {
        let Some(kind) = preset
            .get("command")
            .and_then(|value| value.as_str())
            .and_then(provider_kind_for_command)
        else {
            continue;
        };
        // Several launch variants for one CLI share one usage source. The
        // first preset therefore chooses the provider's position.
        if !order.contains(&kind) {
            order.push(kind);
        }
    }
    Some(order)
}

/// Match Unpeel's command-head convention: the first whitespace-delimited
/// token, reduced to its basename so absolute launch paths also work.
fn provider_kind_for_command(command: &str) -> Option<ProviderKind> {
    let head = command.split_whitespace().next()?;
    let executable = Path::new(head)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(head);
    match executable {
        "codex" => Some(ProviderKind::Codex),
        "claude" => Some(ProviderKind::Claude),
        _ => None,
    }
}

pub fn level_for_used_percent(used: f64, alert_at: f64) -> Level {
    if alert_at > 0.0 && used >= alert_at {
        Level::Alert
    } else if alert_at > 0.0 && used >= alert_at * 0.8 {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// "4.3B" / "1.2M" / "312k" / "980" token formatting.
pub fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000_000 {
        format!("{:.1}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

pub fn compact_usd(amount: f64) -> String {
    if amount >= 100.0 {
        format!("${amount:.0}")
    } else {
        format!("${amount:.2}")
    }
}

/// Read at most the trailing `cap` bytes of a file, aligned to the first
/// complete line. Provider session logs grow large; the fresh evidence is at
/// the tail.
pub fn read_tail(path: &std::path::Path, cap: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(cap);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut raw = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut raw).ok()?;
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    if start > 0 {
        // Drop the partial first line the arbitrary seek produced.
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpeel_presets_filter_deduplicate_and_order_supported_providers() {
        let raw = br#"{
            "presets": [
                {"command": "/opt/tools/claude --dangerously-skip-permissions"},
                {"command": "grok --always-approve"},
                {"command": "codex --yolo", "enabled": true},
                {"command": "claude"},
                {"command": "codex", "enabled": false}
            ]
        }"#;

        assert_eq!(
            provider_order_from_app_state(raw),
            Some(vec![ProviderKind::Claude, ProviderKind::Codex])
        );
    }

    #[test]
    fn an_authoritative_empty_preset_list_selects_no_providers() {
        assert_eq!(
            provider_order_from_app_state(br#"{"presets": []}"#),
            Some(Vec::new())
        );
    }

    #[test]
    fn unusable_state_does_not_override_standalone_provider_defaults() {
        assert_eq!(provider_order_from_app_state(br#"{}"#), None);
        assert_eq!(provider_order_from_app_state(b"not json"), None);

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let absent = std::env::temp_dir().join(format!(
            "unpeel-usage-absent-home-{}-{nonce}",
            std::process::id()
        ));
        assert_eq!(unpeel_preset_provider_order(&absent), None);
    }
}
