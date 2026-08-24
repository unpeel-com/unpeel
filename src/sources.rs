//! Provider model and scan orchestration. Everything is read from local
//! files the tools already write — no API keys, no network, no daemon.

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Alert,
}

/// One gauge/value row inside a provider card.
#[derive(Debug, Clone)]
pub struct Metric {
    pub label: String,
    /// 0..=100 renders a bar; None renders the value only.
    pub percent: Option<f64>,
    pub value: String,
    pub level: Level,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub name: &'static str,
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
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub providers: Vec<Provider>,
}

impl Snapshot {
    pub fn scan(config: &Config) -> Self {
        Self {
            providers: vec![crate::codex::scan(config), crate::claude::scan(config)],
        }
    }

    pub fn alerts(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter_map(|provider| provider.alert.as_deref())
            .collect()
    }

    /// The sidebar status line: alert text when alerting, otherwise the
    /// compact per-provider fragments. Kept short — it renders under a
    /// sidebar row and in a phone list.
    pub fn status_line(&self, alerts_enabled: bool) -> String {
        if alerts_enabled {
            if let Some(alert) = self.alerts().first() {
                return format!("⚠ {alert}");
            }
        }
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

pub fn level_for_used_percent(used: f64, alert_at: f64) -> Level {
    if alert_at > 0.0 && used >= alert_at {
        Level::Alert
    } else if alert_at > 0.0 && used >= alert_at * 0.8 {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// "1.2M" / "312k" / "980" token formatting.
pub fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
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
