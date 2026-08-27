//! Claude Code combines two sources, following OpenUsage's provider model:
//! the existing Claude Code OAuth login supplies real Session, Weekly,
//! model-scoped, and Extra Usage limits, while conversation transcripts under
//! `<config dir>/projects/` supply the machine-local Usage Trend and estimated
//! Today / Yesterday / Last 30 Days spend.
//!
//! Multiple accounts work in both common forms. Separate `CLAUDE_CONFIG_DIR`
//! homes become independent cards. When someone instead uses Claude Code's
//! normal `/logout` + `/login` flow in one shared home, the active account is
//! keyed by email and its last successful, non-secret usage response is kept
//! as a saved card after the login changes. Shared transcripts remain one
//! combined local-history source because they carry no account identity.

use crate::config::Config;
use crate::sources::{
    compact_tokens, compact_usd, read_tail, Level, Metric, Provider, ProviderKind,
};
use crate::timeparse::{compact_duration, now_epoch_secs, parse_epoch_secs};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const DAY_SECS: i64 = 24 * 3_600;
/// Calendar history needs a little slack for time-zone and DST boundaries.
const LOOKBACK_SECS: i64 = 32 * DAY_SECS;
const BLOCK_SECS: i64 = 5 * 3_600;
const WEEK_SECS: i64 = 7 * DAY_SECS;
const TAIL_BYTES: u64 = 8 * 1024 * 1024;
const HISTORY_DAYS: usize = 30;
/// Anthropic rate-limits the live endpoint aggressively. A short cache also
/// means the app's 30-second local-log refresh never becomes API polling.
const LIVE_CACHE_SECS: i64 = 5 * 60;
const LIVE_RETRY_SECS: i64 = 60;
const SAVED_ACCOUNT_RETENTION_SECS: i64 = 90 * DAY_SECS;
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

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
    match raw
        .strip_prefix("~/")
        .and_then(|rest| Some(home()?.join(rest)))
    {
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

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeOAuth {
    access_token: Option<String>,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    expires_at: Option<f64>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
    scopes: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCredentialsFile {
    claude_ai_oauth: Option<ClaudeOAuth>,
}

fn parse_credentials(raw: &str) -> Option<ClaudeOAuth> {
    let credentials: ClaudeCredentialsFile = serde_json::from_str(raw).ok()?;
    let oauth = credentials.claude_ai_oauth?;
    oauth
        .access_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
        .then_some(oauth)
}

fn primary_claude_dir() -> Option<PathBuf> {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .and_then(|raw| raw.split(',').next().map(str::trim).map(expand_home))
        .or_else(|| home().map(|home| home.join(".claude")))
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().unwrap_or_else(|_| left.to_path_buf())
        == right.canonicalize().unwrap_or_else(|_| right.to_path_buf())
}

fn sha256_prefix(raw: &str) -> String {
    Sha256::digest(raw.as_bytes())
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn keychain_services(account: &Account) -> Vec<String> {
    let base = "Claude Code-credentials";
    let primary = primary_claude_dir().is_some_and(|dir| same_path(&dir, &account.dir));
    let mut services = Vec::new();
    if primary {
        if let Ok(raw) = std::env::var("CLAUDE_CONFIG_DIR") {
            let raw = raw.split(',').next().map(str::trim).unwrap_or_default();
            if !raw.is_empty() {
                services.push(format!("{base}-{}", sha256_prefix(raw)));
            }
        }
        services.push(base.to_string());
    } else {
        // Alternate Claude homes conventionally use a path-scoped service.
        services.push(format!(
            "{base}-{}",
            sha256_prefix(&account.dir.to_string_lossy())
        ));
    }
    services
}

#[cfg(target_os = "macos")]
fn keychain_credentials(service: &str) -> Option<ClaudeOAuth> {
    fn run(service: &str, account: Option<&str>) -> Option<String> {
        let mut command = std::process::Command::new("/usr/bin/security");
        command.arg("find-generic-password");
        if let Some(account) = account {
            command.args(["-a", account]);
        }
        let output = command.args(["-s", service, "-w"]).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    let current_user = std::env::var("USER").ok();
    current_user
        .as_deref()
        .and_then(|user| run(service, Some(user)))
        .or_else(|| run(service, None))
        .as_deref()
        .and_then(parse_credentials)
}

#[cfg(not(target_os = "macos"))]
fn keychain_credentials(_service: &str) -> Option<ClaudeOAuth> {
    None
}

/// Claude Code's keychain is the current source of truth on macOS; the
/// credentials file remains the portable and older-install fallback.
fn oauth_credentials(account: &Account) -> Option<ClaudeOAuth> {
    for service in keychain_services(account) {
        if let Some(credentials) = keychain_credentials(&service) {
            return Some(credentials);
        }
    }
    let raw = std::fs::read_to_string(account.dir.join(".credentials.json")).ok()?;
    parse_credentials(&raw)
}

fn title_words(raw: &str) -> String {
    raw.split([' ', '-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn rate_multiplier(tier: &str) -> Option<&str> {
    tier.split(|character: char| !character.is_ascii_alphanumeric())
        .find(|part| {
            part.ends_with('x')
                && part.len() > 1
                && part[..part.len() - 1]
                    .chars()
                    .all(|character| character.is_ascii_digit())
        })
}

fn plan_label(oauth: &ClaudeOAuth) -> Option<String> {
    let subscription = oauth.subscription_type.as_deref()?.trim();
    if subscription.is_empty() {
        return None;
    }
    let plan = title_words(subscription);
    Some(
        oauth
            .rate_limit_tier
            .as_deref()
            .and_then(rate_multiplier)
            .map(|multiplier| format!("{plan} {multiplier}"))
            .unwrap_or(plan),
    )
}

#[derive(Clone, Deserialize, Serialize)]
struct LiveQuota {
    label: String,
    used: f64,
    resets_at: Option<i64>,
    period_secs: i64,
}

#[derive(Clone, Deserialize, Serialize)]
struct ExtraUsage {
    used_usd: f64,
    limit_usd: Option<f64>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct LiveUsage {
    quotas: Vec<LiveQuota>,
    extra: Option<ExtraUsage>,
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str()?.parse::<f64>().ok())
    })
}

fn reset_epoch(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return parse_epoch_secs(text);
    }
    let number = value.as_f64()?;
    Some(if number.abs() < 10_000_000_000.0 {
        number.round() as i64
    } else {
        (number / 1_000.0).round() as i64
    })
}

fn usage_window(body: &Value, key: &str, label: &str, period_secs: i64) -> Option<LiveQuota> {
    let object = body.get(key)?.as_object()?;
    let used = number(object.get("utilization"))?;
    Some(LiveQuota {
        label: label.to_string(),
        used: used.clamp(0.0, 100.0),
        resets_at: reset_epoch(object.get("resets_at")),
        period_secs,
    })
}

fn parse_live_usage(body: &Value) -> LiveUsage {
    let mut usage = LiveUsage::default();
    if let Some(session) = usage_window(body, "five_hour", "Session", BLOCK_SECS) {
        usage.quotas.push(session);
    }
    if let Some(weekly) = usage_window(body, "seven_day", "Weekly", WEEK_SECS) {
        usage.quotas.push(weekly);
    }
    if let Some(sonnet) = usage_window(body, "seven_day_sonnet", "Sonnet", WEEK_SECS) {
        usage.quotas.push(sonnet);
    }
    if let Some(limits) = body.get("limits").and_then(Value::as_array) {
        if let Some(limit) = limits.iter().find(|limit| {
            limit.get("kind").and_then(Value::as_str) == Some("weekly_scoped")
                && limit
                    .pointer("/scope/model/display_name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("Fable"))
        }) {
            if let Some(used) = number(limit.get("percent")) {
                usage.quotas.push(LiveQuota {
                    label: "Fable".into(),
                    used: used.clamp(0.0, 100.0),
                    resets_at: reset_epoch(limit.get("resets_at")),
                    period_secs: WEEK_SECS,
                });
            }
        }
    }
    if let Some(extra) = body.get("extra_usage").and_then(Value::as_object) {
        if extra.get("is_enabled").and_then(Value::as_bool) == Some(true) {
            if let Some(used_cents) = number(extra.get("used_credits")) {
                usage.extra = Some(ExtraUsage {
                    used_usd: (used_cents / 100.0).max(0.0),
                    limit_usd: number(extra.get("monthly_limit"))
                        .filter(|limit| *limit > 0.0)
                        .map(|limit| limit / 100.0),
                });
            }
        }
    }
    usage
}

fn http_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .new_agent()
    })
}

#[derive(Clone, Copy)]
enum LiveFetchError {
    Status(u16),
    Transport,
    InvalidResponse,
}

fn fetch_live_usage(access_token: &str) -> Result<LiveUsage, LiveFetchError> {
    let authorization = format!("Bearer {}", access_token.trim());
    let mut response = http_agent()
        .get(CLAUDE_USAGE_URL)
        .header("Authorization", authorization)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", "claude-code/2.1.69")
        .call()
        .map_err(|_| LiveFetchError::Transport)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(LiveFetchError::Status(status));
    }
    let raw = response
        .body_mut()
        .read_to_string()
        .map_err(|_| LiveFetchError::InvalidResponse)?;
    let body: Value = serde_json::from_str(&raw).map_err(|_| LiveFetchError::InvalidResponse)?;
    Ok(parse_live_usage(&body))
}

#[derive(Clone, Default)]
struct LiveOutcome {
    usage: Option<LiveUsage>,
    plan: Option<String>,
    fetched_at: Option<i64>,
    warning: Option<String>,
}

/// A non-secret snapshot retained across normal `/logout` + `/login` account
/// switches. It deliberately contains no access or refresh token: a logged-out
/// card is historical until that account becomes Claude Code's active login
/// again.
#[derive(Clone, Deserialize, Serialize)]
struct SavedAccount {
    email: String,
    source_dir: String,
    plan: Option<String>,
    usage: LiveUsage,
    fetched_at: i64,
}

#[derive(Default, Deserialize, Serialize)]
struct SavedAccounts {
    accounts: Vec<SavedAccount>,
}

fn account_snapshot_path() -> Option<PathBuf> {
    let base = if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(path)
    } else {
        let home = home()?;
        #[cfg(target_os = "macos")]
        {
            home.join("Library").join("Caches")
        }
        #[cfg(not(target_os = "macos"))]
        {
            home.join(".cache")
        }
    };
    Some(base.join("unpeel-usage").join("claude-accounts.json"))
}

fn load_account_snapshots(now: i64) -> Vec<SavedAccount> {
    let Some(path) = account_snapshot_path() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(mut saved) = serde_json::from_str::<SavedAccounts>(&raw) else {
        return Vec::new();
    };
    saved.accounts.retain(|account| {
        !account.email.trim().is_empty()
            && !account.usage.quotas.is_empty()
            && now.saturating_sub(account.fetched_at) <= SAVED_ACCOUNT_RETENTION_SECS
    });
    saved.accounts
}

fn write_account_snapshots(saved: &SavedAccounts) -> std::io::Result<()> {
    use std::io::Write as _;

    let Some(path) = account_snapshot_path() else {
        return Ok(());
    };
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(saved)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}

fn save_account_snapshot(account: &Account, email: &str, outcome: &LiveOutcome) {
    let (Some(usage), Some(fetched_at)) = (&outcome.usage, outcome.fetched_at) else {
        return;
    };
    let source_dir = source_key(account);
    let mut accounts = load_account_snapshots(fetched_at);
    accounts.retain(|saved| saved.email != email || saved.source_dir != source_dir);
    accounts.push(SavedAccount {
        email: email.to_string(),
        source_dir,
        plan: outcome.plan.clone(),
        usage: usage.clone(),
        fetched_at,
    });
    accounts.sort_by(|left, right| right.fetched_at.cmp(&left.fetched_at));
    let _ = write_account_snapshots(&SavedAccounts { accounts });
}

#[derive(Clone)]
struct CachedLiveOutcome {
    outcome: LiveOutcome,
    retry_at: i64,
}

fn live_cache() -> &'static Mutex<HashMap<String, CachedLiveOutcome>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedLiveOutcome>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn source_key(account: &Account) -> String {
    account
        .dir
        .canonicalize()
        .unwrap_or_else(|_| account.dir.clone())
        .to_string_lossy()
        .into_owned()
}

fn live_cache_key(account: &Account, email: Option<&str>, oauth: Option<&ClaudeOAuth>) -> String {
    let email = email
        .filter(|email| !email.trim().is_empty())
        .unwrap_or("unknown-account");
    let credential = oauth
        .and_then(|oauth| oauth.access_token.as_deref())
        .map(|token| format!("token-{}", sha256_prefix(token)))
        .unwrap_or_else(|| "signed-out".into());
    let identity = format!("{email}\0{credential}");
    format!("{}\0{identity}", source_key(account))
}

fn cached_live_usage(account: &Account, email: Option<&str>, now: i64) -> LiveOutcome {
    // Read identity before consulting the five-minute cache. Logging out and
    // into another account reuses the same Claude directory and Keychain
    // service, so a directory-only key would show the previous account for up
    // to five minutes.
    let oauth = oauth_credentials(account);
    let key = live_cache_key(account, email, oauth.as_ref());
    let previous = {
        let cache = live_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.get(&key) {
            if now < cached.retry_at {
                return cached.outcome.clone();
            }
        }
        cache.get(&key).cloned()
    };

    let Some(oauth) = oauth else {
        let outcome = previous.map_or_else(LiveOutcome::default, |cached| LiveOutcome {
            usage: cached.outcome.usage,
            plan: cached.outcome.plan,
            fetched_at: cached.outcome.fetched_at,
            warning: Some("Claude is logged out; showing the last live update.".into()),
        });
        live_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key,
                CachedLiveOutcome {
                    outcome: outcome.clone(),
                    retry_at: now + LIVE_RETRY_SECS,
                },
            );
        return outcome;
    };

    let plan = plan_label(&oauth).or_else(|| previous.as_ref()?.outcome.plan.clone());
    let warning = if oauth.scopes.as_ref().is_some_and(|scopes| {
        !scopes.is_empty() && !scopes.iter().any(|scope| scope == "user:profile")
    }) {
        Some("Re-login with `claude` to restore live Session and Weekly limits.".to_string())
    } else if oauth
        .expires_at
        .is_some_and(|expires_at| expires_at <= now as f64 * 1_000.0)
    {
        Some("Claude login expired; run `claude` to refresh live limits.".to_string())
    } else {
        None
    };

    let (outcome, retry_secs) = if let Some(warning) = warning {
        (
            LiveOutcome {
                usage: previous
                    .as_ref()
                    .and_then(|cached| cached.outcome.usage.clone()),
                plan,
                fetched_at: previous
                    .as_ref()
                    .and_then(|cached| cached.outcome.fetched_at),
                warning: Some(warning),
            },
            LIVE_RETRY_SECS,
        )
    } else {
        let access_token = oauth.access_token.as_deref().unwrap_or_default();
        match fetch_live_usage(access_token) {
            Ok(usage) => {
                let outcome = LiveOutcome {
                    usage: Some(usage.clone()),
                    plan,
                    fetched_at: Some(now),
                    warning: None,
                };
                if let Some(email) = email {
                    save_account_snapshot(account, email, &outcome);
                }
                (outcome, LIVE_CACHE_SECS)
            }
            Err(error) => {
                let warning = match error {
                    LiveFetchError::Status(401 | 403) => {
                        "Claude login cannot read live limits; run `claude` to sign in again."
                            .to_string()
                    }
                    LiveFetchError::Status(429) => {
                        "Claude live limits are rate limited; using the last update.".to_string()
                    }
                    LiveFetchError::Status(status) => {
                        format!("Claude live limits returned HTTP {status}; local history is still available.")
                    }
                    LiveFetchError::Transport | LiveFetchError::InvalidResponse => {
                        "Claude live limits are temporarily unavailable; local history is still available."
                            .to_string()
                    }
                };
                (
                    LiveOutcome {
                        usage: previous
                            .as_ref()
                            .and_then(|cached| cached.outcome.usage.clone()),
                        plan,
                        fetched_at: previous
                            .as_ref()
                            .and_then(|cached| cached.outcome.fetched_at),
                        warning: Some(warning),
                    },
                    if matches!(error, LiveFetchError::Status(429)) {
                        LIVE_CACHE_SECS
                    } else {
                        LIVE_RETRY_SECS
                    },
                )
            }
        }
    };
    live_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            key,
            CachedLiveOutcome {
                outcome: outcome.clone(),
                retry_at: now + retry_secs,
            },
        );
    outcome
}

fn quota_presentation(quota: &LiveQuota, now: i64) -> (Level, Option<String>, Option<f64>) {
    let used = quota.used.clamp(0.0, 100.0);
    if (100.0 - used).round() <= 0.0 {
        return (Level::Alert, Some("Limit reached".into()), None);
    }
    let Some(resets_at) = quota.resets_at else {
        return (
            if used.round() >= 90.0 {
                Level::Alert
            } else if used.round() >= 80.0 {
                Level::Warn
            } else {
                Level::Ok
            },
            None,
            None,
        );
    };
    let elapsed = now - (resets_at - quota.period_secs);
    let minimum_elapsed = 60.max(quota.period_secs / 100);
    if elapsed < minimum_elapsed || now >= resets_at || used < 5.0 {
        return (
            if used.round() >= 90.0 {
                Level::Alert
            } else if used.round() >= 80.0 {
                Level::Warn
            } else {
                Level::Ok
            },
            None,
            None,
        );
    }

    let projected = used / elapsed as f64 * quota.period_secs as f64;
    let marker = Some((elapsed as f64 / quota.period_secs as f64).clamp(0.0, 1.0));
    if projected <= 90.0 {
        (Level::Ok, None, None)
    } else if projected <= 100.0 {
        let spare = (100.0 - projected).round() as i64;
        if spare >= 1 {
            (Level::Warn, Some(format!("~{spare}% spare")), marker)
        } else {
            (Level::Alert, Some("Near limit".into()), marker)
        }
    } else {
        let rate = projected / quota.period_secs as f64;
        let eta = ((100.0 - used) / rate).round() as i64;
        let remaining = resets_at - now;
        let annotation = if eta > 0 && eta < remaining {
            format!("Limit in {}", compact_duration(eta))
        } else {
            "Running out".into()
        };
        (Level::Alert, Some(annotation), marker)
    }
}

fn quota_metric(quota: &LiveQuota, now: i64) -> Metric {
    let reset = quota
        .resets_at
        .filter(|reset| *reset > now)
        .map(|reset| format!(" · resets {}", compact_duration(reset - now)))
        .unwrap_or_default();
    let (level, annotation, marker) = quota_presentation(quota, now);
    let mut metric = Metric::used_percent(
        quota.label.clone(),
        quota.used,
        format!("{:.0}%{reset}", quota.used),
        level,
    );
    metric.annotation = annotation;
    metric.marker = marker;
    metric
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
            if path.extension().is_none_or(|ext| ext != "jsonl") {
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
                let (estimated_cost, tokens) = message_cost(&model, usage);
                let cost = value
                    .get("costUSD")
                    .and_then(Value::as_f64)
                    .filter(|cost| cost.is_finite() && *cost >= 0.0)
                    .unwrap_or(estimated_cost);
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

#[derive(Clone, Copy, Default)]
struct Totals {
    cost: f64,
    tokens: u64,
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
        // Noon of the previous civil day survives 23h/25h DST transitions.
        start = local_day_start(start - 12 * 3_600);
    }
    newest_first.reverse();
    newest_first
}

fn aggregate_days(entries: &[Entry], starts: &[i64]) -> Vec<Totals> {
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
        slot.cost += entry.cost;
        slot.tokens = slot.tokens.saturating_add(entry.tokens);
    }
    totals
}

fn precise_usd(amount: f64) -> String {
    if amount >= 1_000.0 {
        format!("${:.1}K", amount / 1_000.0)
    } else {
        format!("${amount:.2}")
    }
}

fn history_value(total: Totals) -> String {
    if total.cost <= 0.0 && total.tokens == 0 {
        "No data".into()
    } else {
        format!(
            "{} est · {} tokens",
            precise_usd(total.cost),
            compact_tokens(total.tokens)
        )
    }
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
    let now = now_epoch_secs();
    let emails: Vec<Option<String>> = accounts
        .iter()
        .map(|account| account_email(&account.dir))
        .collect();
    let sources: HashSet<String> = accounts.iter().map(source_key).collect();
    let active: HashSet<(String, String)> = accounts
        .iter()
        .zip(&emails)
        .filter_map(|(account, email)| Some((source_key(account), email.clone()?)))
        .collect();
    let saved_before = load_account_snapshots(now);
    let saved_count = saved_before
        .iter()
        .filter(|saved| {
            sources.contains(&saved.source_dir)
                && !active.contains(&(saved.source_dir.clone(), saved.email.clone()))
        })
        .count();
    let multi = accounts.len().saturating_add(saved_count) > 1;
    let mut providers: Vec<Provider> = accounts
        .iter()
        .zip(emails.iter().cloned())
        .map(|(account, email)| scan_account(config, account, email, multi))
        .collect();

    // A successful active-account refresh may have updated the on-disk
    // records, so reload before adding logged-out account cards.
    providers.extend(
        load_account_snapshots(now)
            .into_iter()
            .filter(|saved| {
                sources.contains(&saved.source_dir)
                    && !active.contains(&(saved.source_dir.clone(), saved.email.clone()))
            })
            .map(|saved| saved_account_provider(saved, now)),
    );
    providers
}

fn scan_account(
    config: &Config,
    account: &Account,
    email: Option<String>,
    multi: bool,
) -> Provider {
    let name = match (&account.short, email.as_deref(), multi) {
        (Some(short), _, true) => format!("Claude Code · {short}"),
        (None, Some(email), true) => format!("Claude Code · {email}"),
        _ => "Claude Code".to_string(),
    };
    // The sidebar fragment names the account only when there is more than one.
    let fragment_name = match (&account.short, email.as_deref(), multi) {
        (Some(short), _, true) => short.clone(),
        (None, Some(email), true) => email.to_string(),
        _ => "Claude".to_string(),
    };
    let now = now_epoch_secs();
    let live = if config.claude.live_usage {
        cached_live_usage(account, email.as_deref(), now)
    } else {
        LiveOutcome::default()
    };
    let mut provider = Provider {
        kind: ProviderKind::Claude,
        name,
        badge: live
            .plan
            .clone()
            .or_else(|| email.clone())
            .unwrap_or_default(),
        present: true,
        metrics: Vec::new(),
        detail: Vec::new(),
        as_of: live.fetched_at,
        alert: None,
        status_fragment: None,
        day_usd: None,
    };
    if live.plan.is_some() {
        if let Some(email) = &email {
            provider.detail.push(("account".into(), email.clone()));
        }
    }
    if let Some(warning) = &live.warning {
        provider.detail.push(("live usage".into(), warning.clone()));
    }

    let entries = recent_entries(&account.dir.join("projects"), now);
    if let Some(latest) = entries.last().map(|entry| entry.at) {
        provider.as_of = Some(provider.as_of.map_or(latest, |live| live.max(latest)));
    }

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

    let mut has_live_quota = false;
    if let Some(usage) = &live.usage {
        for quota in &usage.quotas {
            has_live_quota = true;
            provider.metrics.push(quota_metric(quota, now));
        }
        if let Some(extra) = &usage.extra {
            let level = extra.limit_usd.map_or(Level::Ok, |limit| {
                let used = extra.used_usd / limit * 100.0;
                if used >= 90.0 {
                    Level::Alert
                } else if used >= 80.0 {
                    Level::Warn
                } else {
                    Level::Ok
                }
            });
            provider.metrics.push(Metric::new(
                "Extra Usage",
                format!("{} spent", precise_usd(extra.used_usd)),
                level,
            ));
            if let Some(limit) = extra.limit_usd {
                provider
                    .detail
                    .push(("extra limit".into(), precise_usd(limit)));
            }
        }
    }

    // If live limits are unavailable, keep a clearly labeled local fallback
    // instead of making an estimated dollar budget look like a real quota.
    if !has_live_quota {
        let resets = block
            .map(|start| format!(" · resets {}", compact_duration(start + BLOCK_SECS - now)))
            .unwrap_or_default();
        let mut block_metric = Metric::new(
            "Session spend",
            format!("{} est{resets}", compact_usd(block_cost)),
            block_level,
        );
        block_metric.percent =
            (budget > 0.0).then(|| (block_cost / budget * 100.0).clamp(0.0, 100.0));
        provider.metrics.push(block_metric);
    }

    // The header remains a rolling 24-hour estimate; the card itself follows
    // local calendar days, matching OpenUsage's spend tiles.
    let day_entries: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.at >= now - DAY_SECS)
        .collect();
    let day_cost: f64 = day_entries.iter().map(|entry| entry.cost).sum();
    provider.day_usd = Some(day_cost);

    if let Some(session) = live
        .usage
        .as_ref()
        .and_then(|usage| usage.quotas.iter().find(|quota| quota.label == "Session"))
    {
        provider.status_fragment = Some(format!("{fragment_name} {:.0}%", session.used));
    } else {
        provider.status_fragment = Some(format!("{fragment_name} {}", compact_usd(block_cost)));
    }

    let starts = calendar_starts(now, HISTORY_DAYS);
    let daily = aggregate_days(&entries, &starts);
    let has_history = daily
        .iter()
        .any(|total| total.tokens > 0 || total.cost > 0.0);
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
        let has_tokens = daily.iter().any(|total| total.tokens > 0);
        trend.spark = daily
            .iter()
            .map(|total| {
                if has_tokens {
                    total.tokens as f64
                } else {
                    total.cost
                }
            })
            .collect();
    }
    provider.metrics.push(trend);

    let today = daily.last().copied().unwrap_or_default();
    let yesterday = daily
        .get(daily.len().saturating_sub(2))
        .copied()
        .unwrap_or_default();
    let last_30 = daily.iter().fold(Totals::default(), |mut sum, total| {
        sum.cost += total.cost;
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

    // Per-model 30-day breakdown for the expanded card, largest first.
    let mut by_model: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
    let history_start = starts.first().copied().unwrap_or(now - LOOKBACK_SECS);
    for entry in entries.iter().filter(|entry| entry.at >= history_start) {
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
        provider.detail.push((
            "data dir".into(),
            account.dir.to_string_lossy().into_owned(),
        ));
    }
    provider
}

fn saved_level(used: f64) -> Level {
    if used.round() >= 90.0 {
        Level::Alert
    } else if used.round() >= 80.0 {
        Level::Warn
    } else {
        Level::Ok
    }
}

fn saved_quota_metric(quota: &LiveQuota, now: i64) -> Metric {
    let reset = quota
        .resets_at
        .filter(|reset| *reset > now)
        .map(|reset| format!(" · resets {}", compact_duration(reset - now)))
        .unwrap_or_default();
    Metric::used_percent(
        quota.label.clone(),
        quota.used,
        if reset.is_empty() {
            format!("{:.0}% at last sign-in", quota.used)
        } else {
            format!("{:.0}% saved{reset}", quota.used)
        },
        saved_level(quota.used),
    )
}

fn saved_account_provider(saved: SavedAccount, now: i64) -> Provider {
    let mut metrics: Vec<Metric> = saved
        .usage
        .quotas
        .iter()
        .map(|quota| saved_quota_metric(quota, now))
        .collect();
    if let Some(extra) = &saved.usage.extra {
        metrics.push(Metric::new(
            "Extra Usage",
            format!("{} at last sign-in", precise_usd(extra.used_usd)),
            extra.limit_usd.map_or(Level::Ok, |limit| {
                saved_level(extra.used_usd / limit * 100.0)
            }),
        ));
    }
    let status_fragment = saved
        .usage
        .quotas
        .iter()
        .find(|quota| quota.label == "Session")
        .map(|quota| format!("{} {:.0}% saved", saved.email, quota.used));
    Provider {
        kind: ProviderKind::Claude,
        name: format!("Claude Code · {}", saved.email),
        badge: saved
            .plan
            .map(|plan| format!("{plan} · saved"))
            .unwrap_or_else(|| "Saved".into()),
        present: true,
        metrics,
        detail: vec![
            ("account".into(), saved.email),
            (
                "tracking".into(),
                "Saved limits; sign in to this account to refresh".into(),
            ),
        ],
        as_of: Some(saved.fetched_at),
        alert: None,
        status_fragment,
        day_usd: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_openusage_claude_response_shape() {
        let body = json!({
            "five_hour": {"utilization": 39.0, "resets_at": "2026-08-26T13:20:00Z"},
            "seven_day": {"utilization": 53.0, "resets_at": "2026-08-29T15:00:00Z"},
            "seven_day_sonnet": null,
            "limits": [{
                "kind": "weekly_scoped",
                "scope": {"model": {"display_name": "Fable"}},
                "percent": 99.0,
                "resets_at": "2026-08-29T15:00:00Z"
            }],
            "extra_usage": {
                "is_enabled": true,
                "used_credits": 16521,
                "monthly_limit": 20000
            }
        });

        let usage = parse_live_usage(&body);
        assert_eq!(usage.quotas.len(), 3);
        assert_eq!(usage.quotas[0].label, "Session");
        assert_eq!(usage.quotas[1].label, "Weekly");
        assert_eq!(usage.quotas[2].label, "Fable");
        assert_eq!(usage.quotas[2].used, 99.0);
        let extra = usage.extra.expect("extra usage");
        assert!((extra.used_usd - 165.21).abs() < 0.001);
        assert_eq!(extra.limit_usd, Some(200.0));
    }

    #[test]
    fn formats_plan_without_exposing_credentials() {
        let raw = r#"{
            "claudeAiOauth": {
                "accessToken": "secret-access-token",
                "refreshToken": "secret-refresh-token",
                "subscriptionType": "team",
                "rateLimitTier": "default_claude_team_5x",
                "scopes": ["user:profile"]
            }
        }"#;
        let oauth = parse_credentials(raw).expect("credentials");
        assert_eq!(plan_label(&oauth).as_deref(), Some("Team 5x"));
    }

    #[test]
    fn shared_profile_cache_is_isolated_by_account_email() {
        let account = Account {
            dir: PathBuf::from("/tmp/shared-claude-profile"),
            short: None,
        };
        let first = live_cache_key(&account, Some("first@example.com"), None);
        let second = live_cache_key(&account, Some("second@example.com"), None);
        assert_ne!(first, second);
    }

    #[test]
    fn saved_account_cards_are_explicitly_stale_and_have_no_shared_history() {
        let now = 1_800_000_000;
        let provider = saved_account_provider(
            SavedAccount {
                email: "other@example.com".into(),
                source_dir: "/tmp/shared-claude-profile".into(),
                plan: Some("Max 20x".into()),
                usage: LiveUsage {
                    quotas: vec![LiveQuota {
                        label: "Session".into(),
                        used: 96.0,
                        resets_at: Some(now + 3_600),
                        period_secs: BLOCK_SECS,
                    }],
                    extra: None,
                },
                fetched_at: now - 120,
            },
            now,
        );

        assert_eq!(provider.name, "Claude Code · other@example.com");
        assert!(provider.badge.contains("saved"));
        assert_eq!(provider.metrics[0].value, "96% saved · resets 1h 0m");
        assert!(provider
            .metrics
            .iter()
            .all(|metric| metric.label != "Usage Trend"));
        assert!(provider
            .detail
            .iter()
            .any(|(key, value)| key == "tracking" && value.contains("sign in")));
    }

    #[test]
    fn saved_account_file_contains_no_oauth_secrets() {
        let saved = SavedAccounts {
            accounts: vec![SavedAccount {
                email: "other@example.com".into(),
                source_dir: "/tmp/shared-claude-profile".into(),
                plan: Some("Max 20x".into()),
                usage: LiveUsage::default(),
                fetched_at: 1_800_000_000,
            }],
        };
        let raw = serde_json::to_string(&saved).unwrap();
        assert!(raw.contains("other@example.com"));
        assert!(!raw.contains("accessToken"));
        assert!(!raw.contains("refreshToken"));
    }

    #[test]
    fn pace_states_match_the_reference_card() {
        let now = 1_000_000;
        let session = LiveQuota {
            label: "Session".into(),
            used: 39.0,
            resets_at: Some(now + 2 * 3_600 + 24 * 60),
            period_secs: BLOCK_SECS,
        };
        let weekly = LiveQuota {
            label: "Weekly".into(),
            used: 53.0,
            resets_at: Some(now + 3 * DAY_SECS + 4 * 3_600),
            period_secs: WEEK_SECS,
        };
        let fable = LiveQuota {
            label: "Fable".into(),
            used: 99.0,
            resets_at: weekly.resets_at,
            period_secs: WEEK_SECS,
        };

        assert_eq!(quota_presentation(&session, now), (Level::Ok, None, None));
        let weekly_state = quota_presentation(&weekly, now);
        assert_eq!(weekly_state.0, Level::Warn);
        assert!(weekly_state
            .1
            .as_deref()
            .is_some_and(|note| note.contains("spare")));
        assert!(weekly_state.2.is_some());
        let fable_state = quota_presentation(&fable, now);
        assert_eq!(fable_state.0, Level::Alert);
        assert_eq!(fable_state.1.as_deref(), Some("Limit in 56m"));
        assert!(fable_state.2.is_some());
    }

    #[test]
    fn aggregates_calendar_buckets_oldest_first() {
        let entries = vec![
            Entry {
                at: 50,
                model: "old".into(),
                cost: 8.0,
                tokens: 80,
            },
            Entry {
                at: 150,
                model: "sonnet".into(),
                cost: 1.25,
                tokens: 10,
            },
            Entry {
                at: 250,
                model: "opus".into(),
                cost: 2.5,
                tokens: 20,
            },
            Entry {
                at: 350,
                model: "haiku".into(),
                cost: 3.75,
                tokens: 30,
            },
        ];
        let totals = aggregate_days(&entries, &[100, 200, 300]);
        assert_eq!(totals.len(), 3);
        assert_eq!(totals[0].tokens, 10);
        assert_eq!(totals[1].tokens, 20);
        assert_eq!(totals[2].tokens, 30);
        assert!((totals[2].cost - 3.75).abs() < f64::EPSILON);
    }
}
