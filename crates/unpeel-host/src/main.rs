//! Standalone Unpeel session backend binary.
//!
//! Runs the same entry paths as the desktop app's argv-mode re-invocations,
//! without any Tauri/GUI dependency. Invocation styles:
//!
//! - `unpeel-host __session_host__ <launch-file>` (drop-in for the
//!   self-re-invocation contract in `session_host::spawn_host_process`)
//! - `unpeel-host <launch-file>` (launcher used by the native Swift app; it
//!   spawns the detached `__session_host__` form and exits)
//! - `unpeel-host __mcp__` (unified Unpeel MCP over stdio; this is the
//!   command recorded in `~/.unpeel/mcp/claude-mcp.json` and the Codex
//!   wrapper's `mcp_servers.unpeel-sessions` overrides)
//! - `unpeel-host __transcript__ snapshot|stream <session-id>` reads the
//!   provider transcript as normalized JSON for desktop/iOS remote clients.
//! - `unpeel-host __auto_title__ <session-id>` titles an untitled session
//!   from its provider conversation (fired by the app when a hook capture
//!   changes the session's provider id — an in-tool /resume).
//! - `unpeel-host __restart_agent__ <session-id>` resumes only the known
//!   agent inside a live hosted terminal, preserving the Session and PTY.
//! - `unpeel-host __resume_agent__ <session-id>` performs the shell-only form:
//!   it refuses while any runtime or unrecognized foreground job is active.
//! - `unpeel-host __managed_storage__ <session-id>` reports Host-validated,
//!   runtime-owned storage for provider-neutral cleanup by legacy clients.
//! - `unpeel-host __viewport__ snapshot <session-id>` replays output.bin into a
//!   read-only virtual terminal viewport for remote clients.
//! - `unpeel-host __request_screenshot__ <session-id>` sends the typed,
//!   provider-neutral screenshot-artifact prompt through the safe input path.
//! - `unpeel-host __remote__ [--bind ADDR] [--port N]` runs the remote control
//!   server (HTTPS + WSS over the hosted-session artifacts).
//! - `unpeel-host __remote_stdio__` serves the same Host contract as bounded,
//!   concurrent frames over stdin/stdout for `ssh -T` Controllers.
//! - `unpeel-host __serve__` runs the UI-free Host service embedded in the
//!   desktop app bundle; `unpeel serve` is the public spelling.
//! - With `UNPEEL_SSH_ASKPASS_SECRET` set, this binary is the native app's
//!   narrow local system-SSH askpass helper and prints only that secret.
//! - `unpeel-host __compact_output_journals__` reclaims evicted physical
//!   blocks from stopped legacy Session journals without touching live Hosts.
//!
//! The launch file is the JSON `SessionHostLaunch` written by
//! `session_host::write_launch_file`; the host deletes it after reading.

use unpeel_core::{
    browser_mcp, computer_mcp, direct_path_punch, mcp_gate, mcp_host, relay_probe, remote_attach,
    remote_server, remote_stdio, session_host, terminal_viewport, transcripts,
};

/// Allocator spike (`--features mimalloc`): the shared PTY core keeps every
/// Session's allocations in one process, and macOS libmalloc retains freed
/// small-object pages as dirty magazines after teardown. mimalloc purges
/// freed segments on its own schedule. Off by default; measured, not adopted.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();

    // OpenSSH invokes SSH_ASKPASS as `<program> <prompt>`; the prompt is not
    // stable and must never be interpreted as a normal unpeel-host argv mode.
    if let Some(secret) = std::env::var_os("UNPEEL_SSH_ASKPASS_SECRET") {
        println!("{}", secret.to_string_lossy());
        return;
    }

    if args.as_slice() == [unpeel_serve::service::SERVICE_ARG] {
        if let Err(error) = unpeel_serve::service::run(|event| println!("{event}")) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.as_slice() == [unpeel_serve::service::WORKSPACE_WORKER_ARG] {
        if let Err(error) = unpeel_serve::service::run_workspace_worker(|event| println!("{event}"))
        {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(session_host::COMPACT_OUTPUT_JOURNALS_ARG) {
        if args.len() != 1 {
            eprintln!(
                "usage: unpeel-host {}",
                session_host::COMPACT_OUTPUT_JOURNALS_ARG
            );
            std::process::exit(2);
        }
        match session_host::compact_exited_output_journals() {
            Ok(summary) => println!(
                "scanned={} compacted={} logical_bytes_evicted={}",
                summary.scanned, summary.compacted, summary.logical_bytes_evicted
            ),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if args.first().map(String::as_str) == Some(mcp_host::MCP_HOST_ARG) {
        if let Err(error) = mcp_host::run_stdio() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(browser_mcp::BROWSER_MCP_ARG) {
        if let Err(error) = browser_mcp::run_stdio() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(mcp_gate::MCP_GATE_ARG) {
        args.remove(0);
        let kind = args.first().map(String::as_str).unwrap_or_default();
        if let Err(error) = mcp_gate::run_stdio(kind) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    // Runtime-local compatibility aliases keep persisted MCP configurations
    // from older Unpeel builds working without teaching this binary provider
    // argv spellings.
    if let Some(kind) = args
        .first()
        .and_then(|argument| unpeel_core::integrations::legacy_mcp_gate_kind(argument))
    {
        if let Err(error) = mcp_gate::run_stdio(kind) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(browser_mcp::BROWSER_CLEANUP_ARG) {
        args.remove(0);
        if let Err(error) = browser_mcp::run_cleanup(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(computer_mcp::COMPUTER_CLEANUP_ARG) {
        args.remove(0);
        if let Err(error) = computer_mcp::run_cleanup(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some("__managed_storage__") {
        args.remove(0);
        let session_id = args.first().cloned().unwrap_or_default();
        if session_id.is_empty() || args.len() != 1 {
            eprintln!("usage: unpeel-host __managed_storage__ <session-id>");
            std::process::exit(2);
        }
        match unpeel_core::session_ops::managed_storage_path_for_session(&session_id) {
            Some(path) => println!("{path}"),
            None => println!(),
        }
        return;
    }

    // `unpeel-host __resume__ <session-id> [--fresh]` — print the
    // relaunch command a restart of this session should run, as JSON. The
    // native app calls this instead of duplicating the resume tiers; the
    // logic itself lives in unpeel-core::session_ops::relaunch_command.
    if args.first().map(String::as_str) == Some("__resume__") {
        args.remove(0);
        let session_id = args.first().cloned().unwrap_or_default();
        if session_id.is_empty() {
            eprintln!("usage: unpeel-host __resume__ <session-id> [--fresh]");
            std::process::exit(2);
        }
        let mode = unpeel_core::session_ops::RelaunchMode::Restart {
            force_fresh: args.iter().any(|a| a == "--fresh"),
        };
        // unpeel-host keeps serde_json out of the binary; a one-field JSON
        // object needs only string escaping.
        fn json_string(value: &str) -> String {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('"');
            for ch in value.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        match unpeel_core::session_ops::relaunch_command(&session_id, mode) {
            Ok(command) => {
                let markers = unpeel_core::resume::resume_failure_markers(&command)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|marker| json_string(&marker))
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{{\"command\":{},\"failure_markers\":[{}]}}",
                    json_string(&command),
                    markers
                );
                return;
            }
            Err(error) => {
                eprintln!("{{\"error\":{}}}", json_string(&error));
                std::process::exit(1);
            }
        }
    }

    if args.first().map(String::as_str) == Some("__restart_agent__") {
        args.remove(0);
        let session_id = args.first().cloned().unwrap_or_default();
        if session_id.is_empty() || args.len() != 1 {
            eprintln!("usage: unpeel-host __restart_agent__ <session-id>");
            std::process::exit(2);
        }
        match unpeel_core::session_ops::restart_agent(&session_id) {
            Ok(()) => {
                println!("{{\"restarted\":true}}");
                return;
            }
            Err(error) => {
                eprintln!("agent restart failed: {error}");
                std::process::exit(1);
            }
        }
    }

    if args.first().map(String::as_str) == Some("__resume_agent__") {
        args.remove(0);
        let session_id = args.first().cloned().unwrap_or_default();
        if session_id.is_empty() || args.len() != 1 {
            eprintln!("usage: unpeel-host __resume_agent__ <session-id>");
            std::process::exit(2);
        }
        match unpeel_core::session_ops::resume_agent(&session_id) {
            Ok(()) => {
                println!("{{\"resumed\":true}}");
                return;
            }
            Err(error) => {
                eprintln!("agent resume failed: {error}");
                std::process::exit(1);
            }
        }
    }

    if args.first().map(String::as_str) == Some("__transcript__") {
        args.remove(0);
        if let Err(error) = transcripts::run_cli(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some("__request_screenshot__") {
        args.remove(0);
        let Some(session_id) = args.first() else {
            eprintln!("usage: unpeel-host __request_screenshot__ <session-id>");
            std::process::exit(2);
        };
        if let Err(error) = unpeel_core::session_input::request_screenshot(session_id) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        println!("{{\"accepted\":true}}");
        return;
    }

    if args.first().map(String::as_str) == Some("__auto_title__") {
        args.remove(0);
        let Some(session_id) = args.first() else {
            eprintln!("usage: unpeel-host __auto_title__ <session-id>");
            std::process::exit(1);
        };
        // Best-effort by design: untitleable (settled, no transcript yet,
        // nothing normalizable) simply exits 0.
        transcripts::auto_title_session_from_transcript(session_id);
        return;
    }

    if args.first().map(String::as_str) == Some(direct_path_punch::PUNCH_ARG) {
        args.remove(0);
        if let Err(error) = direct_path_punch::run_cli(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(relay_probe::RELAY_PROBE_ARG) {
        args.remove(0);
        if let Err(error) = relay_probe::run_cli(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(remote_server::REMOTE_ARG) {
        args.remove(0);
        if let Err(error) = remote_server::run_cli(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(remote_stdio::REMOTE_STDIO_ARG) {
        args.remove(0);
        if !args.is_empty() {
            eprintln!("usage: unpeel-host {}", remote_stdio::REMOTE_STDIO_ARG);
            std::process::exit(2);
        }
        if let Err(error) = remote_stdio::run_stdio() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if args.first().map(String::as_str) == Some(remote_attach::REMOTE_ATTACH_ARG) {
        args.remove(0);
        std::process::exit(remote_attach::run_cli(&args));
    }

    // `__check_local_url__ <url>...` — re-verify detected local-site URLs
    // against the CURRENT probe rules. The native app shells out here so the
    // "is this a real openable site" logic stays single-sourced in Rust:
    // session hosts are long-lived processes running whatever detection code
    // they started with, so the display layer re-checks with today's rules
    // before showing anything. Prints one `<url>\t<true|false>` line per arg.
    if args.first().map(String::as_str) == Some("__check_local_url__") {
        args.remove(0);
        for url in &args {
            let ok = unpeel_core::local_urls::url_is_openable_site(url);
            println!("{url}\t{ok}");
        }
        return;
    }

    // `__local_site_server__ <url>` — resolve the process serving a detected
    // local-site URL: prints `<pid>\t<command>\t<session-id-or-->`, or exits 1
    // when nothing listens. `__stop_local_site_server__ <url>` resolves and
    // SIGTERMs in one step, but only a session-owned server — never the
    // user's own infra on the same port.
    if args.first().map(String::as_str) == Some("__local_site_server__") {
        match args
            .get(1)
            .and_then(|url| unpeel_core::local_urls::server_for_url(url))
        {
            Some(server) => {
                println!(
                    "{}\t{}\t{}",
                    server.pid,
                    server.command,
                    server.session_id.as_deref().unwrap_or("-")
                );
            }
            None => std::process::exit(1),
        }
        return;
    }
    if args.first().map(String::as_str) == Some("__stop_local_site_server__") {
        let result = args
            .get(1)
            .ok_or_else(|| "usage: __stop_local_site_server__ <url>".to_string())
            .and_then(|url| unpeel_core::local_urls::stop_server_for_url(url));
        match result {
            Ok(server) => println!("stopped {} (pid {})", server.command, server.pid),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `__apps__ list` — central-catalog Unpeel Apps whose binary resolves on
    // the Host's PATH, as JSON for native's "Apps you can add" section.
    if args.first().map(String::as_str) == Some("__apps__") {
        args.remove(0);
        match args.first().map(String::as_str) {
            Some("list") => {
                println!("{}", unpeel_core::apps_mcp::installable_apps_json());
            }
            _ => {
                eprintln!("usage: unpeel-host __apps__ list");
                std::process::exit(2);
            }
        }
        return;
    }

    if args.first().map(String::as_str) == Some("__viewport__") {
        args.remove(0);
        if let Err(error) = terminal_viewport::run_cli(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    // One-shot session grid metrics for the workspace multiplexer: the Mac
    // app proxies GET /mobile/metrics for a scoped LOCAL workspace by running
    // this against that workspace's UNPEEL_HOME — the same read the shared
    // router's metrics route performs in-process for its own home. Prints the
    // wire-shape camelCase JSON on stdout; non-zero exit with the message on
    // stderr.
    if args.first().map(String::as_str) == Some("__metrics__") {
        args.remove(0);
        let Some(session_id) = args.first() else {
            eprintln!("usage: unpeel-host __metrics__ <session-id>");
            std::process::exit(2);
        };
        match unpeel_core::controller_api::read_session_metrics(session_id) {
            Ok(metrics) => {
                // Hand-rolled to keep serde_json out of the binary (see the
                // MCP config note above); the id is already validated to a
                // safe charset by read_session_metrics.
                println!(
                    "{{\"sessionID\":\"{}\",\"columns\":{},\"rows\":{},\"outputOffset\":{},\"capturedAtUnixMs\":{}}}",
                    metrics.session_id,
                    metrics.columns,
                    metrics.rows,
                    metrics.output_offset,
                    metrics.captured_at_unix_ms
                );
            }
            Err(error) => {
                eprintln!("{}", error.message);
                std::process::exit(1);
            }
        }
        return;
    }

    if matches!(
        args.first().map(String::as_str),
        Some(unpeel_core::pty_core::PTY_CORE_ARG)
    ) {
        args.remove(0);
        if let Err(error) = unpeel_core::pty_core::run_from_args(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if matches!(
        args.first().map(String::as_str),
        Some(session_host::SESSION_HOST_ARG)
    ) {
        args.remove(0);
        if let Err(error) = session_host::run_from_args(&args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(launch_file) = args.first() {
        if let Err(error) = session_host::spawn_host_process_from_launch_file(launch_file) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(error) = session_host::run_from_args(&args) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
