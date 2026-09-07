//! Durable, independent activity reported by runtime hooks. One marker per
//! activity avoids read/modify/write races between concurrent subagents. The
//! reporter broadcasts its ordinary hook after an atomic create or removal;
//! the Host reconciles this snapshot even when HTTP delivery was missed.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

pub struct BackgroundHookActivity {
    pub id: String,
    pub started_at: SystemTime,
}

/// Read only the current managed runtime's markers. Invalid/partial files
/// never grant lifecycle authority; an unreadable directory is an error so a
/// transient I/O failure cannot falsely finish already-known background work.
pub fn read_background_hook_activity(
    session_dir: &Path,
    generation: u64,
) -> io::Result<Vec<BackgroundHookActivity>> {
    let directory = session_dir
        .join("background-hooks")
        .join(generation.to_string());
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut activities = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json")
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if metadata.len() > 1024 {
            continue;
        }
        let mut bytes = Vec::new();
        file.take(1024).read_to_end(&mut bytes)?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let Some(id) = value.get("activity_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if id.is_empty()
            || id.len() > 160
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || path.file_stem().and_then(|stem| stem.to_str()) != Some(id)
            || value
                .get("unpeel_runtime_generation")
                .and_then(serde_json::Value::as_u64)
                != Some(generation)
        {
            continue;
        }
        activities.push(BackgroundHookActivity {
            id: id.to_owned(),
            started_at: metadata.modified()?,
        });
    }
    Ok(activities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_markers_require_matching_safe_identity_and_generation() {
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("background-hooks/3");
        fs::create_dir_all(&markers).unwrap();
        for (file, id, generation) in [
            ("child", "child", 3),
            ("stale", "stale", 2),
            ("different", "mismatch", 3),
            ("unsafe", "../escape", 3),
        ] {
            fs::write(
                markers.join(format!("{file}.json")),
                serde_json::json!({
                    "activity_id": id, "unpeel_runtime_generation": generation,
                })
                .to_string(),
            )
            .unwrap();
        }
        fs::write(markers.join("partial.json"), "{").unwrap();
        fs::write(markers.join(".child.123"), "{}").unwrap();
        let activities = read_background_hook_activity(dir.path(), 3).unwrap();
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].id, "child");
        assert!(read_background_hook_activity(dir.path(), 4)
            .unwrap()
            .is_empty());
    }
}
