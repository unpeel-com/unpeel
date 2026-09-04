//! Scriptable workspace settings over the same raw, locked app-state path as
//! the app and TUI settings panels.
//!
//! Parsing is deliberately separate from the local persistence adapter. The
//! command enum can later be dispatched through `settings.workspace.set` for
//! a remote Host without changing the public CLI grammar or value semantics.

use serde_json::{Map, Value};

use unpeel_core::state::{BrowserAccess, ComputerAccess, McpNonChildWriteAccess};

const SETTINGS_USAGE: &str = "usage: unpeel settings list|get <key>|set <key> <value> [--json]";

pub const HELP: &str = "\
usage: unpeel settings list [--json]
       unpeel settings get <key> [--json]
       unpeel settings set <key> <value> [--json]

Script this workspace's allowlisted settings:
  experimental_features.sessions_mcp   true | false
  experimental_features.browser_mcp    true | false
  experimental_features.computer_use   true | false   (alias: computer_use)
  browser_default_access               on | ask | off
  mcp_nonchild_write_access            ask | allow | deny
  computer_access                      ask | allow | off
  mcp_worktree_access                  true | false
  mcp_auto_add_browser_screenshots     true | false
  auto_stop_archive_minutes            0 | 30 | 60 | 120 | 240 | 480 | 1440
  sidebar_stopped_limit                0 | 3 | 5 | 10 | 15 | 25
  theme                                system | light | dark

The Sessions, Browser, and Computer MCP gates are captured when a Session launches.
Start or restart a Session after changing any of them. Computer use also needs
computer_access != off and a ready adapter on the Host.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingKey {
    SessionsMcp,
    BrowserMcp,
    ComputerUse,
    BrowserDefaultAccess,
    McpNonchildWriteAccess,
    ComputerAccess,
    McpWorktreeAccess,
    McpAutoAddBrowserScreenshots,
    AutoStopArchiveMinutes,
    SidebarStoppedLimit,
    Theme,
}

/// The same value sets the Host's `settings.workspace.set` whitelist accepts
/// (`controller_host.rs`) — keep them in sync.
const AUTO_STOP_MINUTE_OPTIONS: [u64; 7] = [0, 30, 60, 120, 240, 480, 1440];
const SIDEBAR_LIMIT_OPTIONS: [u64; 6] = [0, 3, 5, 10, 15, 25];

impl SettingKey {
    const ALL: [Self; 11] = [
        Self::SessionsMcp,
        Self::BrowserMcp,
        Self::ComputerUse,
        Self::BrowserDefaultAccess,
        Self::McpNonchildWriteAccess,
        Self::ComputerAccess,
        Self::McpWorktreeAccess,
        Self::McpAutoAddBrowserScreenshots,
        Self::AutoStopArchiveMinutes,
        Self::SidebarStoppedLimit,
        Self::Theme,
    ];

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "experimental_features.sessions_mcp" => Ok(Self::SessionsMcp),
            "experimental_features.browser_mcp" => Ok(Self::BrowserMcp),
            // Short spelling used by unpeel-apple:docs/plans/computer-use-release.md and
            // the Box recipe; the stored key is the nested experimental gate.
            "experimental_features.computer_use" | "computer_use" => Ok(Self::ComputerUse),
            "browser_default_access" => Ok(Self::BrowserDefaultAccess),
            "mcp_nonchild_write_access" => Ok(Self::McpNonchildWriteAccess),
            "computer_access" => Ok(Self::ComputerAccess),
            "mcp_worktree_access" => Ok(Self::McpWorktreeAccess),
            "mcp_auto_add_browser_screenshots" => Ok(Self::McpAutoAddBrowserScreenshots),
            "auto_stop_archive_minutes" => Ok(Self::AutoStopArchiveMinutes),
            "sidebar_stopped_limit" => Ok(Self::SidebarStoppedLimit),
            "theme" => Ok(Self::Theme),
            _ => Err(format!(
                "unknown setting {raw:?}; supported keys: {}",
                Self::ALL
                    .iter()
                    .map(|key| key.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::SessionsMcp => "experimental_features.sessions_mcp",
            Self::BrowserMcp => "experimental_features.browser_mcp",
            Self::ComputerUse => "experimental_features.computer_use",
            Self::BrowserDefaultAccess => "browser_default_access",
            Self::McpNonchildWriteAccess => "mcp_nonchild_write_access",
            Self::ComputerAccess => "computer_access",
            Self::McpWorktreeAccess => "mcp_worktree_access",
            Self::McpAutoAddBrowserScreenshots => "mcp_auto_add_browser_screenshots",
            Self::AutoStopArchiveMinutes => "auto_stop_archive_minutes",
            Self::SidebarStoppedLimit => "sidebar_stopped_limit",
            Self::Theme => "theme",
        }
    }

    fn allowed_values(self) -> &'static str {
        match self {
            Self::SessionsMcp | Self::BrowserMcp | Self::ComputerUse => "true or false",
            Self::BrowserDefaultAccess => "on, ask, or off",
            Self::McpNonchildWriteAccess => "ask, allow, or deny",
            Self::ComputerAccess => "ask, allow, or off",
            Self::McpWorktreeAccess | Self::McpAutoAddBrowserScreenshots => "true or false",
            Self::AutoStopArchiveMinutes => "0, 30, 60, 120, 240, 480, or 1440 (0 = off)",
            Self::SidebarStoppedLimit => "0, 3, 5, 10, 15, or 25",
            Self::Theme => "system, light, or dark",
        }
    }

    fn parse_value(self, raw: &str) -> Result<Value, String> {
        let normalized = raw.trim().to_ascii_lowercase();
        let value = match self {
            Self::SessionsMcp | Self::BrowserMcp | Self::ComputerUse => match normalized.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return self.invalid_value(raw),
            },
            Self::BrowserDefaultAccess => match normalized.as_str() {
                "on" | "ask" | "off" => Value::String(normalized),
                _ => return self.invalid_value(raw),
            },
            Self::McpNonchildWriteAccess => match normalized.as_str() {
                "ask" | "allow" | "deny" => Value::String(normalized),
                _ => return self.invalid_value(raw),
            },
            Self::ComputerAccess => match normalized.as_str() {
                "ask" | "allow" | "off" => Value::String(normalized),
                _ => return self.invalid_value(raw),
            },
            Self::McpWorktreeAccess | Self::McpAutoAddBrowserScreenshots => {
                match normalized.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => return self.invalid_value(raw),
                }
            }
            Self::AutoStopArchiveMinutes => match normalized.parse::<u64>() {
                Ok(minutes) if AUTO_STOP_MINUTE_OPTIONS.contains(&minutes) => {
                    Value::Number(minutes.into())
                }
                _ => return self.invalid_value(raw),
            },
            Self::SidebarStoppedLimit => match normalized.parse::<u64>() {
                Ok(limit) if SIDEBAR_LIMIT_OPTIONS.contains(&limit) => Value::Number(limit.into()),
                _ => return self.invalid_value(raw),
            },
            Self::Theme => match normalized.as_str() {
                "system" | "light" | "dark" => Value::String(normalized),
                _ => return self.invalid_value(raw),
            },
        };
        Ok(value)
    }

    fn invalid_value<T>(self, raw: &str) -> Result<T, String> {
        Err(format!(
            "invalid value {raw:?} for {}; expected {}",
            self.name(),
            self.allowed_values()
        ))
    }

    /// Effective value using the same defaults/fail-closed behavior as the
    /// Session launch and MCP consumers, while leaving the stored document
    /// completely untyped.
    fn read(self, state: &Map<String, Value>) -> Value {
        match self {
            Self::SessionsMcp => Value::Bool(read_experiment(state, "sessions_mcp", true)),
            Self::BrowserMcp => Value::Bool(read_experiment(state, "browser_mcp", true)),
            // Same default as the Rust launch gate
            // (`computer_mcp::requested_from_app_state`): off until set.
            Self::ComputerUse => Value::Bool(read_experiment(state, "computer_use", false)),
            Self::BrowserDefaultAccess => {
                let access = match state.get("browser_default_access") {
                    None => BrowserAccess::default(),
                    Some(Value::String(raw)) => BrowserAccess::from_state_str(raw),
                    Some(_) => BrowserAccess::Off,
                };
                Value::String(access.as_state_str().into())
            }
            Self::McpNonchildWriteAccess => {
                let access = state
                    .get("mcp_nonchild_write_access")
                    .and_then(Value::as_str)
                    .map(McpNonChildWriteAccess::from_state_str)
                    .unwrap_or_default();
                Value::String(access.as_state_str().into())
            }
            Self::ComputerAccess => {
                // The Host stores `computer_default_access`; the short-lived
                // minor-13 `computer_access` spelling is a read fallback only.
                let access = state
                    .get("computer_default_access")
                    .or_else(|| state.get("computer_access"))
                    .and_then(Value::as_str)
                    .map(ComputerAccess::from_state_str)
                    .unwrap_or_default();
                Value::String(access.as_state_str().into())
            }
            Self::McpWorktreeAccess => Value::Bool(
                state
                    .get("mcp_worktree_access")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
            Self::McpAutoAddBrowserScreenshots => Value::Bool(
                state
                    .get("mcp_auto_add_browser_screenshots")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            ),
            Self::AutoStopArchiveMinutes => Value::Number(
                state
                    .get("auto_stop_archive_minutes")
                    .and_then(Value::as_u64)
                    .filter(|minutes| AUTO_STOP_MINUTE_OPTIONS.contains(minutes))
                    .unwrap_or(unpeel_serve::auto_archive::DEFAULT_MINUTES)
                    .into(),
            ),
            Self::SidebarStoppedLimit => Value::Number(
                state
                    .get("sidebar_stopped_limit")
                    .and_then(Value::as_u64)
                    .filter(|limit| SIDEBAR_LIMIT_OPTIONS.contains(limit))
                    .unwrap_or(unpeel_serve::sessions::DEFAULT_SIDEBAR_STOPPED_LIMIT)
                    .into(),
            ),
            Self::Theme => {
                let theme = state
                    .get("theme")
                    .and_then(Value::as_str)
                    .filter(|value| matches!(*value, "system" | "light" | "dark"))
                    .unwrap_or("system");
                Value::String(theme.into())
            }
        }
    }

    fn write(self, state: &mut Map<String, Value>, value: Value) -> Result<(), String> {
        match self {
            Self::SessionsMcp | Self::BrowserMcp | Self::ComputerUse => {
                let features = state
                    .entry("experimental_features")
                    .or_insert_with(|| Value::Object(Map::new()));
                let Some(features) = features.as_object_mut() else {
                    return Err(
                        "experimental_features is not an object; refusing to replace it".into(),
                    );
                };
                let nested_key = match self {
                    Self::SessionsMcp => "sessions_mcp",
                    Self::BrowserMcp => "browser_mcp",
                    Self::ComputerUse => "computer_use",
                    _ => unreachable!(),
                };
                features.insert(nested_key.into(), value);
            }
            Self::BrowserDefaultAccess => {
                state.insert("browser_default_access".into(), value);
            }
            Self::McpNonchildWriteAccess => {
                state.insert("mcp_nonchild_write_access".into(), value);
            }
            Self::ComputerAccess => {
                state.insert("computer_default_access".into(), value);
                // Never leave the legacy spelling able to shadow the Host's key.
                state.remove("computer_access");
            }
            Self::McpWorktreeAccess => {
                state.insert("mcp_worktree_access".into(), value);
            }
            Self::McpAutoAddBrowserScreenshots => {
                state.insert("mcp_auto_add_browser_screenshots".into(), value);
            }
            Self::AutoStopArchiveMinutes => {
                state.insert("auto_stop_archive_minutes".into(), value);
            }
            Self::SidebarStoppedLimit => {
                state.insert("sidebar_stopped_limit".into(), value);
            }
            Self::Theme => {
                state.insert("theme".into(), value);
            }
        }
        Ok(())
    }
}

fn read_experiment(state: &Map<String, Value>, key: &str, fallback: bool) -> bool {
    state
        .get("experimental_features")
        .and_then(Value::as_object)
        .and_then(|features| features.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SettingsCommand {
    List,
    Get(SettingKey),
    Set { key: SettingKey, value: Value },
}

fn parse_command(args: &[String]) -> Result<SettingsCommand, String> {
    match args.first().map(String::as_str) {
        None | Some("list") if args.len() <= 1 => Ok(SettingsCommand::List),
        Some("get") if args.len() == 2 => Ok(SettingsCommand::Get(SettingKey::parse(&args[1])?)),
        Some("set") if args.len() == 3 => {
            let key = SettingKey::parse(&args[1])?;
            let value = key.parse_value(&args[2])?;
            Ok(SettingsCommand::Set { key, value })
        }
        Some("list" | "get" | "set") | None => Err(SETTINGS_USAGE.into()),
        Some(other) => Err(format!(
            "unknown settings subcommand {other:?}; {SETTINGS_USAGE}"
        )),
    }
}

pub fn run(args: &[String], json: bool) -> Result<(), String> {
    match parse_command(args)? {
        SettingsCommand::List => {
            let state = unpeel_core::app_state::load()?;
            let state = state
                .as_object()
                .ok_or_else(|| "app-state.json is not an object".to_string())?;
            if json {
                let values: Map<String, Value> = SettingKey::ALL
                    .iter()
                    .map(|key| (key.name().into(), key.read(state)))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string(&values).map_err(|e| e.to_string())?
                );
            } else {
                for key in SettingKey::ALL {
                    println!("{:<42} {}", key.name(), display_value(&key.read(state)));
                }
            }
            Ok(())
        }
        SettingsCommand::Get(key) => {
            let state = unpeel_core::app_state::load()?;
            let state = state
                .as_object()
                .ok_or_else(|| "app-state.json is not an object".to_string())?;
            print_value(key, key.read(state), json)
        }
        SettingsCommand::Set { key, value } => {
            let effective = value.clone();
            unpeel_core::app_state::edit(|state| key.write(state, value))?;
            print_value(key, effective, json)
        }
    }
}

fn print_value(key: SettingKey, value: Value, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "key": key.name(),
                "value": value,
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        println!("{}", display_value(&value));
    }
    Ok(())
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_access_reads_fail_closed_and_missing_uses_shipped_default() {
        let missing = Map::new();
        assert_eq!(
            SettingKey::BrowserDefaultAccess.read(&missing),
            Value::String("on".into())
        );

        for malformed in [Value::String("surprise".into()), Value::Bool(true)] {
            let mut state = Map::new();
            state.insert("browser_default_access".into(), malformed);
            assert_eq!(
                SettingKey::BrowserDefaultAccess.read(&state),
                Value::String("off".into())
            );
        }
    }

    #[test]
    fn nested_experiment_edit_preserves_unknown_fields() {
        let mut state = serde_json::json!({
            "future_top_level": { "kept": true },
            "experimental_features": {
                "future_gate": "untouched",
                "browser_mcp": true,
            }
        })
        .as_object()
        .unwrap()
        .clone();

        SettingKey::SessionsMcp
            .write(&mut state, Value::Bool(false))
            .unwrap();
        assert_eq!(state["future_top_level"]["kept"], true);
        assert_eq!(state["experimental_features"]["future_gate"], "untouched");
        assert_eq!(state["experimental_features"]["browser_mcp"], true);
        assert_eq!(state["experimental_features"]["sessions_mcp"], false);
    }

    #[test]
    fn command_parser_rejects_unknown_keys_and_values_before_writing() {
        let unknown_key = vec!["set".into(), "future.setting".into(), "true".into()];
        assert!(parse_command(&unknown_key)
            .unwrap_err()
            .contains("unknown setting"));

        let invalid_access = vec![
            "set".into(),
            "browser_default_access".into(),
            "allow".into(),
        ];
        assert!(parse_command(&invalid_access)
            .unwrap_err()
            .contains("expected on, ask, or off"));
    }
}
