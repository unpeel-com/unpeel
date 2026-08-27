//! User configuration: refresh cadence and alert thresholds. Lives at
//! `~/.config/unpeel-usage/config.toml`; a missing file means defaults, and
//! the file is written once with commented defaults so thresholds are
//! discoverable without documentation.

use crate::theme::ThemePreference;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub refresh_secs: u64,
    pub theme: ThemePreference,
    pub alerts: Alerts,
    pub claude: Claude,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Claude {
    /// Fetch the subscription's real Session/Weekly/model limits using the
    /// existing Claude Code OAuth login. Local history remains available when
    /// this is disabled or the endpoint cannot be reached.
    pub live_usage: bool,
    /// Extra Claude Code config directories beyond the auto-detected ones —
    /// one per additional account, each a `CLAUDE_CONFIG_DIR`-style dir with
    /// its own `projects/` transcripts. `~` expands to the home directory.
    pub dirs: Vec<String>,
}

impl Default for Claude {
    fn default() -> Self {
        Self {
            live_usage: true,
            dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Alerts {
    /// Notify when a quota is at least 80% used or its current pace projects
    /// that it will run out before reset.
    pub close_to_limit: bool,
    /// Notify when a bounded quota reaches 100%.
    pub limit_reached: bool,
    /// Notify when a previously constrained quota is usable again.
    pub available_again: bool,
    /// Codex window utilization (percent) at or above which to mark the card.
    pub codex_used_percent: f64,
    /// Codex credits balance (USD) at or below which to alert. 0 disables.
    pub credits_low_usd: f64,
    /// Claude estimated 5h-block spend (USD) at or above which to alert.
    /// 0 disables — Claude records no quota locally, so this is a personal
    /// budget line, not a provider limit.
    pub claude_block_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertOption {
    CloseToLimit,
    LimitReached,
    AvailableAgain,
}

impl AlertOption {
    pub const ALL: [Self; 3] = [Self::CloseToLimit, Self::LimitReached, Self::AvailableAgain];

    pub const fn label(self) -> &'static str {
        match self {
            Self::CloseToLimit => "Close to a limit",
            Self::LimitReached => "Limit reached",
            Self::AvailableAgain => "Available again",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::CloseToLimit => "At 80% used or projected to run out",
            Self::LimitReached => "When a quota reaches 100% used",
            Self::AvailableAgain => "After a constrained quota resets",
        }
    }
}

impl Alerts {
    pub const fn enabled(self, option: AlertOption) -> bool {
        match option {
            AlertOption::CloseToLimit => self.close_to_limit,
            AlertOption::LimitReached => self.limit_reached,
            AlertOption::AvailableAgain => self.available_again,
        }
    }

    pub fn toggle(&mut self, option: AlertOption) {
        match option {
            AlertOption::CloseToLimit => self.close_to_limit = !self.close_to_limit,
            AlertOption::LimitReached => self.limit_reached = !self.limit_reached,
            AlertOption::AvailableAgain => self.available_again = !self.available_again,
        }
    }

    pub fn enabled_count(self) -> usize {
        AlertOption::ALL
            .into_iter()
            .filter(|option| self.enabled(*option))
            .count()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_secs: 30,
            theme: ThemePreference::Auto,
            alerts: Alerts::default(),
            claude: Claude::default(),
        }
    }
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            close_to_limit: false,
            limit_reached: false,
            available_again: false,
            codex_used_percent: 85.0,
            credits_low_usd: 20.0,
            claude_block_usd: 0.0,
        }
    }
}

const DEFAULT_FILE: &str = "\
# unpeel-usage configuration. Delete this file to restore defaults.

# Seconds between background rescans. Claude live responses are independently
# cached for five minutes to respect the provider endpoint.
refresh_secs = 30

# Color palette: \"auto\" asks the terminal, with \"light\" and \"dark\" available
# when its background color cannot be detected correctly.
theme = \"auto\"

[alerts]
# Unpeel notifications. All are off until enabled in the `a` dialog or here.
close_to_limit = false
limit_reached = false
available_again = false
# Codex window utilization (percent) at or above which to mark the card.
codex_used_percent = 85.0
# Codex credits balance (USD) at or below which to alert. 0 disables.
credits_low_usd = 20.0
# Claude estimated 5h-block spend (USD) at or above which to alert.
# Claude records no quota locally, so this is a personal budget. 0 disables.
claude_block_usd = 0.0

[claude]
# Read real subscription limits through Claude Code's existing OAuth login.
# Set false for a fully offline, transcript-only dashboard.
live_usage = true
# Extra Claude Code accounts. The default account (~/.claude, or
# $CLAUDE_CONFIG_DIR) and any ~/.claude-* directory holding transcripts are
# detected automatically; list additional config dirs here, e.g.
# dirs = [\"~/claude-accounts/work\"]
dirs = []
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_notifications_are_opt_in() {
        let alerts = Alerts::default();
        assert_eq!(alerts.enabled_count(), 0);
        for option in AlertOption::ALL {
            assert!(!alerts.enabled(option));
        }
    }

    #[test]
    fn alert_options_toggle_independently() {
        let mut alerts = Alerts::default();
        alerts.toggle(AlertOption::AvailableAgain);
        assert!(alerts.available_again);
        assert!(!alerts.close_to_limit);
        assert!(!alerts.limit_reached);
    }

    #[test]
    fn legacy_master_switch_does_not_opt_users_in() {
        let config: Config = toml::from_str("[alerts]\nenabled = true\n").unwrap();
        assert_eq!(config.alerts.enabled_count(), 0);
    }
}
