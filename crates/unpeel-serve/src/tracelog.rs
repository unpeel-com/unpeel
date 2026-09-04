//! Timestamped append to `~/.unpeel/hooks/trace.log` for serve-side
//! components. Undated trace lines proved undiagnosable in the 2026-08-30
//! relay-disconnect investigation (a reconnect burst could not be placed in
//! time); every serve trace line now carries UTC time + unix seconds.

pub(crate) fn trace(component: &str, message: &str) {
    // This is also the only output channel for the app-launched service,
    // whose stdout/stderr are intentionally disconnected. Reuse the core
    // trace writer so diagnostics are directory-safe and bounded at 10 MiB
    // with one rotated `trace.log.1` generation.
    unpeel_core::hook_assets::append_trace_log_line(&format!("{} {component} {message}", stamp()));
}

fn stamp() -> String {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let seconds_of_day = unix % 86_400;
    format!(
        "[{:02}:{:02}:{:02}Z {unix}]",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}
