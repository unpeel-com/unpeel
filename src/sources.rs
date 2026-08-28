//! Provider model and scan orchestration. Codex, Claude, Grok, and Muse
//! history comes from local tool files; Claude and Grok can additionally
//! reuse their CLI logins for live subscription limits. No pasted API keys
//! and no daemon.

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
    Grok,
    Muse,
    /// Aggregate row scoped to the project from which the App was launched.
    CurrentProject,
    /// Aggregate row synthesized after all configured providers are scanned.
    Total,
}

/// One local calendar month's token count. Providers expose these raw rows so
/// the final Total usage card can aggregate them without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthUsage {
    pub year: i32,
    pub month: u8,
    pub tokens: u64,
}

/// One project's machine-local token history. Paths are normalized to the
/// nearest Git root when it still exists, otherwise kept as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectUsage {
    pub path: PathBuf,
    pub monthly_tokens: Vec<MonthUsage>,
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
    /// Machine-local token history, grouped by local calendar month.
    pub monthly_tokens: Vec<MonthUsage>,
    /// The same history attributed to project roots when a provider records
    /// a working directory. Entries without trustworthy project evidence are
    /// still included in `monthly_tokens`, but not guessed into this list.
    pub project_usage: Vec<ProjectUsage>,
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
                ProviderKind::Grok => providers.push(crate::grok::scan(config)),
                ProviderKind::Muse => providers.push(crate::muse::scan()),
                ProviderKind::CurrentProject | ProviderKind::Total => {}
            }
        }
        if !providers.is_empty() {
            let now = crate::timeparse::now_epoch_secs();
            if let Some(current) = current_project_provider(&providers, now) {
                providers.push(current);
            }
            providers.push(total_provider(&providers, now));
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
        .unwrap_or_else(|| {
            vec![
                ProviderKind::Codex,
                ProviderKind::Claude,
                ProviderKind::Grok,
                ProviderKind::Muse,
            ]
        })
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
        "grok" => Some(ProviderKind::Grok),
        "muse" => Some(ProviderKind::Muse),
        _ => None,
    }
}

/// Aggregate timestamped entries into sorted local calendar-month rows.
pub fn aggregate_monthly_tokens<I>(entries: I) -> Vec<MonthUsage>
where
    I: IntoIterator<Item = (i64, u64)>,
{
    let mut totals = std::collections::BTreeMap::<(i32, u8), u64>::new();
    for (at, tokens) in entries {
        if tokens == 0 {
            continue;
        }
        let Some(key) = local_year_month(at) else {
            continue;
        };
        let total = totals.entry(key).or_default();
        *total = total.saturating_add(tokens);
    }
    totals
        .into_iter()
        .map(|((year, month), tokens)| MonthUsage {
            year,
            month,
            tokens,
        })
        .collect()
}

/// Aggregate timestamped token entries by their recorded project. The caller
/// supplies `None` when the source has no trustworthy working-directory
/// evidence; those tokens remain part of provider totals but stay unattributed.
pub fn aggregate_project_usage<I>(entries: I) -> Vec<ProjectUsage>
where
    I: IntoIterator<Item = (i64, u64, Option<PathBuf>)>,
{
    let mut projects =
        std::collections::BTreeMap::<PathBuf, std::collections::BTreeMap<(i32, u8), u64>>::new();
    for (at, tokens, path) in entries {
        if tokens == 0 {
            continue;
        }
        let (Some(path), Some(month)) = (path, local_year_month(at)) else {
            continue;
        };
        let total = projects
            .entry(normalize_project_path(&path))
            .or_default()
            .entry(month)
            .or_default();
        *total = total.saturating_add(tokens);
    }
    projects
        .into_iter()
        .map(|(path, months)| ProjectUsage {
            path,
            monthly_tokens: months
                .into_iter()
                .map(|((year, month), tokens)| MonthUsage {
                    year,
                    month,
                    tokens,
                })
                .collect(),
        })
        .collect()
}

/// Reduce a directory to the nearest enclosing Git project without invoking
/// Git for every session log. Worktree `.git` files and normal `.git`
/// directories are both accepted.
pub fn normalize_project_path(path: &Path) -> PathBuf {
    let normalized = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for candidate in normalized.ancestors() {
        let marker = candidate.join(".git");
        if marker.is_file() {
            if let Ok(pointer) = std::fs::read_to_string(&marker) {
                if let Some(git_dir) = pointer.trim().strip_prefix("gitdir: ") {
                    let git_dir = PathBuf::from(git_dir);
                    let git_dir = if git_dir.is_absolute() {
                        git_dir
                    } else {
                        candidate.join(git_dir)
                    };
                    if git_dir
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name == "worktrees")
                    {
                        if let Some(main_root) = git_dir
                            .parent()
                            .and_then(Path::parent)
                            .and_then(Path::parent)
                        {
                            return std::fs::canonicalize(main_root)
                                .unwrap_or_else(|_| main_root.to_path_buf());
                        }
                    }
                }
            }
        }
        if marker.exists() {
            return candidate.to_path_buf();
        }
    }
    normalized
}

/// Build the fixed newest-first month window used by the Total usage table.
fn month_window(now: i64, count: usize) -> Vec<(i32, u8)> {
    let Some((mut year, mut month)) = local_year_month(now) else {
        return Vec::new();
    };
    let mut months = Vec::with_capacity(count);
    for _ in 0..count {
        months.push((year, month));
        if month == 1 {
            year -= 1;
            month = 12;
        } else {
            month -= 1;
        }
    }
    months
}

fn filled_months(totals: &std::collections::BTreeMap<(i32, u8), u64>, now: i64) -> Vec<MonthUsage> {
    month_window(now, 12)
        .into_iter()
        .map(|(year, month)| MonthUsage {
            year,
            month,
            tokens: totals.get(&(year, month)).copied().unwrap_or(0),
        })
        .collect()
}

fn summary_metric(monthly_tokens: &[MonthUsage]) -> Metric {
    let current = monthly_tokens.first().map_or(0, |usage| usage.tokens);
    Metric::new(
        "This month",
        if current == 0 {
            "No data".into()
        } else {
            format!("{} tokens", compact_tokens(current))
        },
        Level::Ok,
    )
}

fn current_project_provider(providers: &[Provider], now: i64) -> Option<Provider> {
    let project = normalize_project_path(&std::env::current_dir().ok()?);
    let mut totals = std::collections::BTreeMap::<(i32, u8), u64>::new();
    for usage in providers
        .iter()
        .flat_map(|provider| provider.project_usage.iter())
        .filter(|usage| usage.path == project)
        .flat_map(|usage| usage.monthly_tokens.iter())
    {
        let total = totals.entry((usage.year, usage.month)).or_default();
        *total = total.saturating_add(usage.tokens);
    }
    let monthly_tokens = filled_months(&totals, now);
    let badge = project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_string();
    Some(Provider {
        kind: ProviderKind::CurrentProject,
        name: "Current project".into(),
        badge,
        present: true,
        metrics: vec![summary_metric(&monthly_tokens)],
        detail: vec![("path".into(), project.display().to_string())],
        as_of: providers.iter().filter_map(|provider| provider.as_of).max(),
        alert: None,
        status_fragment: None,
        day_usd: None,
        monthly_tokens,
        project_usage: Vec::new(),
    })
}

fn total_provider(providers: &[Provider], now: i64) -> Provider {
    let mut totals = std::collections::BTreeMap::<(i32, u8), u64>::new();
    for usage in providers
        .iter()
        .filter(|provider| provider.kind != ProviderKind::CurrentProject)
        .flat_map(|provider| provider.monthly_tokens.iter())
    {
        let total = totals.entry((usage.year, usage.month)).or_default();
        *total = total.saturating_add(usage.tokens);
    }
    let monthly_tokens = filled_months(&totals, now);
    let mut project_totals =
        std::collections::BTreeMap::<PathBuf, std::collections::BTreeMap<(i32, u8), u64>>::new();
    for project in providers
        .iter()
        .filter(|provider| provider.kind != ProviderKind::CurrentProject)
        .flat_map(|provider| provider.project_usage.iter())
    {
        for usage in &project.monthly_tokens {
            let total = project_totals
                .entry(project.path.clone())
                .or_default()
                .entry((usage.year, usage.month))
                .or_default();
            *total = total.saturating_add(usage.tokens);
        }
    }
    let project_usage = project_totals
        .into_iter()
        .map(|(path, months)| ProjectUsage {
            path,
            monthly_tokens: months
                .into_iter()
                .map(|((year, month), tokens)| MonthUsage {
                    year,
                    month,
                    tokens,
                })
                .collect(),
        })
        .collect();
    Provider {
        kind: ProviderKind::Total,
        name: "Total usage".into(),
        badge: String::new(),
        present: true,
        metrics: vec![summary_metric(&monthly_tokens)],
        detail: vec![(
            "meaning".into(),
            "processed tokens, including cached context".into(),
        )],
        as_of: providers.iter().filter_map(|provider| provider.as_of).max(),
        alert: None,
        status_fragment: None,
        day_usd: None,
        monthly_tokens,
        project_usage,
    }
}

#[cfg(unix)]
fn local_year_month(epoch: i64) -> Option<(i32, u8)> {
    let timestamp = epoch as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return None;
    }
    let year = local.tm_year.checked_add(1_900)?;
    let month = u8::try_from(local.tm_mon.checked_add(1)?).ok()?;
    Some((year, month))
}

#[cfg(not(unix))]
fn local_year_month(epoch: i64) -> Option<(i32, u8)> {
    let days = epoch.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some((i32::try_from(year).ok()?, u8::try_from(month).ok()?))
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
                {"command": "muse"},
                {"command": "codex --yolo", "enabled": true},
                {"command": "claude"},
                {"command": "codex", "enabled": false}
            ]
        }"#;

        assert_eq!(
            provider_order_from_app_state(raw),
            Some(vec![
                ProviderKind::Claude,
                ProviderKind::Grok,
                ProviderKind::Muse,
                ProviderKind::Codex,
            ])
        );
    }

    #[test]
    fn monthly_aggregation_saturates_and_groups_the_local_calendar_month() {
        let now = crate::timeparse::now_epoch_secs();
        let rows = aggregate_monthly_tokens([(now, 120), (now, 80)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens, 200);
    }

    #[test]
    fn project_aggregation_uses_the_git_root_and_keeps_unattributed_tokens_out() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "unpeel-usage-project-{}-{nonce}",
            std::process::id()
        ));
        let nested = root.join("nested/folder");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        let now = crate::timeparse::now_epoch_secs();

        let rows = aggregate_project_usage([
            (now, 120, Some(nested)),
            (now, 80, Some(root.clone())),
            (now, 999, None),
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, std::fs::canonicalize(&root).unwrap());
        assert_eq!(rows[0].monthly_tokens[0].tokens, 200);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn total_provider_does_not_count_the_current_project_summary_twice() {
        let now = crate::timeparse::now_epoch_secs();
        let month = aggregate_monthly_tokens([(now, 120)]);
        let provider = |kind| Provider {
            kind,
            name: "test".into(),
            badge: String::new(),
            present: true,
            metrics: Vec::new(),
            detail: Vec::new(),
            as_of: None,
            alert: None,
            status_fragment: None,
            day_usd: None,
            monthly_tokens: month.clone(),
            project_usage: Vec::new(),
        };

        let total = total_provider(
            &[
                provider(ProviderKind::Codex),
                provider(ProviderKind::CurrentProject),
            ],
            now,
        );

        assert_eq!(total.monthly_tokens[0].tokens, 120);
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
