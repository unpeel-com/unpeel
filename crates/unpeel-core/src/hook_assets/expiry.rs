//! A timed-out hook turn stays expired across Host restarts. The watermark
//! only fences opening events from the same launch; newer hooks still win.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

#[derive(Deserialize, Serialize)]
struct Expiry {
    runtime_generation: u64,
    through: SystemTime,
}

fn read(path: &Path) -> Option<Expiry> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn hook_turn_expired(dir: &Path, generation: u64, event_at: SystemTime) -> bool {
    read(&dir.join("hook-expiry.json"))
        .is_some_and(|expiry| expiry.runtime_generation == generation && event_at <= expiry.through)
}

pub fn record_hook_expiry(dir: &Path, generation: u64, through: SystemTime) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    let path = dir.join("hook-expiry.json");
    let _lock = crate::app_state::lock_exclusive(&path)?;
    if read(&path).is_some_and(|old| {
        old.runtime_generation > generation
            || (old.runtime_generation == generation && old.through >= through)
    }) {
        return Ok(());
    }
    let bytes = serde_json::to_string(&Expiry {
        runtime_generation: generation,
        through,
    })
    .map_err(|error| error.to_string())?;
    super::write_file_atomic(&path, &bytes, "hook expiry")?;
    crate::state_bus::announce(crate::state_bus::Change::Lifecycle, None);
    Ok(())
}
