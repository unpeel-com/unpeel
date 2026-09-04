//! Timestamped append to `~/.unpeel/hooks/trace.log` for serve-side
//! components. Undated trace lines proved undiagnosable in the 2026-08-30
//! relay-disconnect investigation (a reconnect burst could not be placed in
//! time); every serve trace line now carries UTC time + unix seconds.

pub(crate) fn trace(component: &str, message: &str) {
    let line = format!("{} {component} {message}", stamp());
    // This is also the only output channel for the app-launched service,
    // whose stdout/stderr are intentionally disconnected. Reuse the core
    // trace writer so diagnostics are directory-safe and bounded at 10 MiB
    // with one rotated `trace.log.1` generation.
    #[cfg(not(test))]
    unpeel_core::hook_assets::append_trace_log_line(&line);
    // Unit tests run with the developer's real `UNPEEL_HOME` (unset), so the
    // fake platform adapters and relay-recovery replays used to append their
    // lines to the operator's own `~/.unpeel/hooks/trace.log` (seen 2026-09-04).
    // Every unit test in this crate shares one per-process scratch trace log
    // instead; process cases set an isolated `UNPEEL_HOME` and keep the real
    // writer above.
    #[cfg(test)]
    test_sink::append(&line);
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

#[cfg(test)]
mod test_sink {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn path() -> &'static PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let dir = std::env::temp_dir()
                .join(format!("unpeel-serve-unit-trace-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            dir.join("trace.log")
        })
    }

    pub(super) fn append(line: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path())
        {
            let _ = writeln!(file, "{line}");
        }
    }
}
