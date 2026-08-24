//! User configuration: refresh cadence and alert thresholds. Lives at
//! `~/.config/unpeel-usage/config.toml`; a missing file means defaults, and
//! the file is written once with commented defaults so thresholds are
//! discoverable without documentation.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub refresh_secs: u64,
    pub alerts: Alerts,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Alerts {
    pub enabled: bool,
    /// Codex window utilization (percent) at or above which to alert.
    pub codex_used_percent: f64,
    /// Codex credits balance (USD) at or below which to alert. 0 disables.
    pub credits_low_usd: f64,
    /// Claude estimated 5h-block spend (USD) at or above which to alert.
    /// 0 disables — Claude records no quota locally, so this is a personal
    /// budget line, not a provider limit.
    pub claude_block_usd: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_secs: 30,
            alerts: Alerts::default(),
        }
    }
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            enabled: true,
            codex_used_percent: 85.0,
            credits_low_usd: 20.0,
            claude_block_usd: 0.0,
        }
    }
}

const DEFAULT_FILE: &str = "\
# unpeel-usage configuration. Delete this file to restore defaults.

# Seconds between background rescans of local usage data.
refresh_secs = 30

[alerts]
# Master switch; `a` in the app toggles this for the session only.
enabled = true
# Codex window utilization (percent) at or above which to alert.
codex_used_percent = 85.0
# Codex credits balance (USD) at or below which to alert. 0 disables.
credits_low_usd = 20.0
# Claude estimated 5h-block spend (USD) at or above which to alert.
# Claude records no quota locally, so this is a personal budget. 0 disables.
claude_block_usd = 0.0
";

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("unpeel-usage").join("config.toml"))
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => toml::from_str(&raw).unwrap_or_default(),
            Err(_) => {
                // First run: seed the commented default file, best-effort.
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                    let _ = std::fs::write(&path, DEFAULT_FILE);
                }
                Self::default()
            }
        }
    }
}
