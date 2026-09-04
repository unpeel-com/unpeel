//! Provider-neutral environment gate for runtimes whose CLI only supports
//! persistent MCP configuration. A runtime package may install an entry that
//! always starts this gate, then scope the generic grant variables around its
//! managed invocation. Outside a granted hosted Session the endpoint remains
//! valid but advertises no tools.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub const MCP_GATE_ARG: &str = "__mcp_gate__";
pub const SESSIONS_KIND: &str = "sessions";
pub const BROWSER_KIND: &str = "browser";
pub const COMPUTER_KIND: &str = "computer";
/// The unified server behind one gate entry: enabled when *any* domain grant
/// is present; the registration mask and Session manifest must both grant a
/// domain before the server advertises it. Replaces per-domain entries in new
/// installs.
pub const UNIFIED_KIND: &str = "unified";

pub const SESSIONS_ENABLED_ENV: &str = "UNPEEL_SESSIONS_MCP_ENABLED";
pub const BROWSER_ENABLED_ENV: &str = "UNPEEL_BROWSER_MCP_ENABLED";
pub const COMPUTER_ENABLED_ENV: &str = "UNPEEL_COMPUTER_MCP_ENABLED";
const PROTOCOL_VERSION_FALLBACK: &str = "2025-06-18";

pub fn run_stdio(kind: &str) -> Result<(), String> {
    // Runtime adapters may recognize only their own pre-migration grant
    // aliases. The generic gate stays provider-neutral and still requires a
    // valid hosted Session identity below before advertising any tools.
    let sessions_granted = env_grant(SESSIONS_ENABLED_ENV)
        || crate::integrations::legacy_mcp_gate_granted(SESSIONS_KIND);
    let browser_granted = env_grant(BROWSER_ENABLED_ENV)
        || crate::integrations::legacy_mcp_gate_granted(BROWSER_KIND);
    let computer_granted = env_grant(COMPUTER_ENABLED_ENV)
        || crate::integrations::legacy_mcp_gate_granted(COMPUTER_KIND);
    let unified_domains = crate::mcp_host::McpDomainMask {
        sessions: sessions_granted,
        agents: sessions_granted,
        workspace: sessions_granted,
        artifacts: sessions_granted,
        browser: browser_granted,
        computer: computer_granted,
        // Apps discovery rides any granted domain: it is read-only manifest
        // discovery, present by default wherever the unified server is live.
        apps: sessions_granted || browser_granted || computer_granted,
        skills: sessions_granted || browser_granted || computer_granted,
    };
    let (granted, server_name) = match kind {
        UNIFIED_KIND => (
            sessions_granted || browser_granted || computer_granted,
            "unpeel",
        ),
        SESSIONS_KIND => (sessions_granted, "unpeel-sessions"),
        BROWSER_KIND => (browser_granted, "unpeel-browser"),
        _ => return Err(format!("Unknown gated MCP kind: {kind}")),
    };

    let enabled = granted && crate::mcp_host::self_session_id().is_some();
    if enabled {
        return match kind {
            UNIFIED_KIND => crate::mcp_host::run_stdio_with_domains(unified_domains),
            // The sessions kind delegates to the same unified server, bounded
            // to that one domain: stale per-domain entries keep working after
            // a binary update without inheriting other manifest grants.
            SESSIONS_KIND => {
                crate::mcp_host::run_stdio_with_domains(crate::mcp_host::McpDomainMask {
                    sessions: true,
                    agents: true,
                    workspace: true,
                    artifacts: true,
                    browser: false,
                    computer: false,
                    apps: false,
                    skills: false,
                })
            }
            BROWSER_KIND => crate::browser_mcp::run_stdio(),
            _ => unreachable!(),
        };
    }

    run_empty_stdio(server_name)
}

fn env_grant(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}

fn run_empty_stdio(server_name: &str) -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| format!("Failed to read gated MCP stdin: {error}"))?;
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(response) = empty_response(server_name, &message) else {
            continue;
        };
        let body = serde_json::to_string(&response)
            .map_err(|error| format!("Failed to encode gated MCP response: {error}"))?;
        let mut out = stdout.lock();
        out.write_all(body.as_bytes())
            .and_then(|_| out.write_all(b"\n"))
            .and_then(|_| out.flush())
            .map_err(|error| format!("Failed to write gated MCP response: {error}"))?;
    }
    Ok(())
}

fn empty_response(server_name: &str, message: &Value) -> Option<Value> {
    let id = match message.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => return None,
    };
    let method = message.get("method").and_then(Value::as_str)?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let modern = crate::mcp_host::request_uses_modern_meta(&params) || method == "server/discover";
    if modern {
        if let Err(error) = crate::mcp_host::validate_modern_request_meta(&params) {
            return Some(json!({ "jsonrpc": "2.0", "id": id, "error": error }));
        }
    }
    let result = match method {
        "initialize" if !modern => {
            let protocol_version = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION_FALLBACK);
            json!({
                "protocolVersion": protocol_version,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": server_name,
                    "version": env!("CARGO_PKG_VERSION"),
                },
            })
        }
        "server/discover" => json!({
            "supportedVersions": [crate::mcp_host::MODERN_PROTOCOL_VERSION],
            "capabilities": { "tools": {} },
            "instructions": "This Unpeel MCP registration is disabled for the current process.",
            "ttlMs": 0,
            "cacheScope": "private",
        }),
        "ping" => json!({}),
        "tools/list" => {
            if modern {
                json!({ "tools": [], "ttlMs": 0, "cacheScope": "private" })
            } else {
                json!({ "tools": [] })
            }
        }
        "tools/call" => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": "This Unpeel MCP server is not enabled for the current session.",
                },
            }));
        }
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}"),
                },
            }));
        }
    };
    Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": if modern {
            crate::mcp_host::modern_result_for_server(result, server_name)
        } else {
            result
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_gate_advertises_no_tools() {
        let response = empty_response(
            "unpeel-sessions",
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .expect("response");
        assert_eq!(response["result"]["tools"], json!([]));
    }

    #[test]
    fn initialize_echoes_the_requested_protocol() {
        let response = empty_response(
            "unpeel-browser",
            &json!({
                "jsonrpc":"2.0",
                "id":"init",
                "method":"initialize",
                "params":{"protocolVersion":"2026-01-01"}
            }),
        )
        .expect("response");
        assert_eq!(response["result"]["protocolVersion"], "2026-01-01");
        assert_eq!(response["result"]["serverInfo"]["name"], "unpeel-browser");
    }
}
