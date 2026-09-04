//! Read-only production-SSH probe for the Controller ↔ Host protocol.
//!
//! Run with:
//! `cargo run -p unpeel-core --example ssh_host_probe -- ssh://studio`

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use unpeel_core::remote_session_backend::{
    RemoteActivityState, RemoteBootstrapSnapshot, RemoteSessionBackend, RemoteSessionStatus,
};
use unpeel_core::ssh_connection::{SshHostConnection, SshTarget};

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_HEADROOM: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
struct Config {
    target: String,
    json: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Arguments {
    Help,
    Run(Config),
}

fn main() -> ExitCode {
    let arguments = match parse_arguments(std::env::args_os().skip(1).map(|argument| {
        argument
            .into_string()
            .map_err(|_| "arguments must be valid UTF-8".to_string())
    })) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!(
                "ssh_host_probe: {}\n\n{}",
                safe_field(&message, 4096),
                usage()
            );
            return ExitCode::from(2);
        }
    };

    match arguments {
        Arguments::Help => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Arguments::Run(config) => match run(config) {
            Ok(output) => match io::stdout().lock().write_all(output.as_bytes()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!(
                        "ssh_host_probe: write output: {}",
                        safe_field(&error.to_string(), 4096)
                    );
                    ExitCode::FAILURE
                }
            },
            Err(message) => {
                eprintln!("ssh_host_probe: {}", safe_field(&message, 4096));
                ExitCode::FAILURE
            }
        },
    }
}

fn usage() -> &'static str {
    "Usage: ssh_host_probe [--json] ssh://alias\n\
     \n\
     Opens the production system-SSH Host gateway, performs one read-only\n\
     bootstrap request, validates protocol compatibility, and exits. SSH port,\n\
     identity, ProxyJump, and other policy belong in ~/.ssh/config."
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = Result<String, String>>,
) -> Result<Arguments, String> {
    let mut json = false;
    let mut target = None;

    for argument in arguments {
        let argument = argument?;
        match argument.as_str() {
            "-h" | "--help" => return Ok(Arguments::Help),
            "--json" if !json => json = true,
            "--json" => return Err("--json may only be supplied once".to_string()),
            value if value.starts_with('-') => {
                return Err(format!("unknown option: {value}"));
            }
            value if target.is_none() => target = Some(value.to_string()),
            value => return Err(format!("unexpected second Host target: {value}")),
        }
    }

    let target = target.ok_or_else(|| "missing ssh:// Host target".to_string())?;
    Ok(Arguments::Run(Config { target, json }))
}

fn run(config: Config) -> Result<String, String> {
    let target = SshTarget::parse(&config.target).map_err(|error| error.to_string())?;
    let target_uri = target.uri().to_string();
    let backend = RemoteSessionBackend::with_timeouts(
        Arc::new(SshHostConnection::new(target)),
        PROBE_TIMEOUT,
        OUTPUT_HEADROOM,
    );

    // Keep process ownership local to this probe. Even an error path closes
    // and reaps the system-SSH child before returning to the shell.
    let bootstrap = backend.bootstrap();
    backend.disconnect();
    let bootstrap = bootstrap.map_err(|error| error.to_string())?;

    if config.json {
        let mut output = serde_json::to_string_pretty(&bootstrap.raw)
            .map_err(|error| format!("encode bootstrap JSON: {error}"))?;
        output.push('\n');
        Ok(output)
    } else {
        Ok(render_summary(&target_uri, &bootstrap.snapshot))
    }
}

fn render_summary(target_uri: &str, bootstrap: &RemoteBootstrapSnapshot) -> String {
    let mut output = String::new();
    let display_name = bootstrap.host_name.as_deref().unwrap_or(target_uri);
    let _ = writeln!(output, "Host: {}", safe_field(display_name, 160));
    if display_name != target_uri {
        let _ = writeln!(output, "Target: {}", safe_field(target_uri, 255));
    }
    if let Some(host_id) = bootstrap.host_id.as_deref() {
        let _ = writeln!(output, "Host ID: {}", safe_field(host_id, 160));
    }
    match &bootstrap.host_protocol {
        Some(protocol) => {
            let _ = writeln!(
                output,
                "Protocol: mobile {}; Host {}.{} ({} capabilities)",
                bootstrap.protocol_version,
                protocol.major_version,
                protocol.minor_version,
                protocol.capabilities.len()
            );
        }
        None => {
            let _ = writeln!(
                output,
                "Protocol: mobile {} (legacy Host capabilities unknown)",
                bootstrap.protocol_version
            );
        }
    }
    let _ = writeln!(output, "Sessions: {}", bootstrap.sessions.len());
    for session in &bootstrap.sessions {
        let state = format!(
            "{}/{}",
            status_name(session.status),
            activity_name(session.activity)
        );
        let _ = writeln!(
            output,
            "  - {} [{}] ({})",
            safe_field(&session.title, 160),
            safe_field(&state, 80),
            safe_field(&session.id, 160)
        );
    }
    output
}

fn status_name(status: RemoteSessionStatus) -> &'static str {
    match status {
        RemoteSessionStatus::Running => "running",
        RemoteSessionStatus::Exited => "exited",
    }
}

fn activity_name(activity: RemoteActivityState) -> &'static str {
    match activity {
        RemoteActivityState::Starting => "starting",
        RemoteActivityState::Working => "working",
        RemoteActivityState::Blocked => "blocked",
        RemoteActivityState::Done => "done",
        RemoteActivityState::Idle => "idle",
        RemoteActivityState::Unknown => "unknown",
    }
}

fn safe_field(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    for _ in 0..max_chars {
        let Some(character) = chars.next() else {
            return output;
        };
        output.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn arguments(values: &[&str]) -> Result<Arguments, String> {
        parse_arguments(values.iter().map(|value| Ok((*value).to_string())))
    }

    fn bootstrap() -> Value {
        json!({
            "protocolVersion": 1,
            "hostProtocol": {
                "majorVersion": 1,
                "minorVersion": 99,
                "capabilities": ["host.bootstrap", "session.output.read"],
            },
            "macID": "host-1",
            "macName": "Studio",
            "folders": [],
            "projects": [],
            "presets": [],
            "sessions": [{
                "id": "session-1",
                "projectID": "project-1",
                "title": "Research",
                "command": "claude",
                "createdAtUnixMs": 1,
                "status": "running",
                "activity": "working",
            }],
            "capturedAtUnixMs": 2
        })
    }

    #[test]
    fn arguments_are_small_and_strict() {
        assert_eq!(
            arguments(&["ssh://studio", "--json"]),
            Ok(Arguments::Run(Config {
                target: "ssh://studio".to_string(),
                json: true,
            }))
        );
        assert_eq!(arguments(&["--help"]), Ok(Arguments::Help));
        assert!(arguments(&[]).unwrap_err().contains("missing"));
        assert!(arguments(&["--wat"]).unwrap_err().contains("unknown"));
        assert!(arguments(&["ssh://one", "ssh://two"])
            .unwrap_err()
            .contains("second"));
    }

    #[test]
    fn human_output_is_bounded_and_drops_terminal_controls() {
        let mut value = bootstrap();
        value["sessions"][0]["title"] = "bad\u{1b}[31m\nname".into();
        let parsed: RemoteBootstrapSnapshot = serde_json::from_value(value).unwrap();
        let rendered = render_summary("ssh://studio", &parsed);
        assert!(rendered.contains("Host: Studio\nTarget: ssh://studio"));
        assert!(rendered.contains("bad [31m name"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains("\nname"));

        let diagnostic = safe_field("\u{1b}]0;remote title\u{7}failed\nnext", 4096);
        assert!(diagnostic.chars().all(|character| !character.is_control()));
        assert!(diagnostic.contains("failed next"));

        let bounded = safe_field(&"x".repeat(5_000), 4_096);
        assert_eq!(bounded.chars().count(), 4_097);
        assert!(bounded.ends_with('…'));
    }
}
