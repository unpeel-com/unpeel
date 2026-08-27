use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use unpeel_tui_kit::ExplorerEntry;

use crate::install::APP_ID;

pub struct ContextReporter {
    session_dir: Option<PathBuf>,
    last_context: Option<(PathBuf, Option<PathBuf>)>,
}

impl ContextReporter {
    pub fn detect() -> Self {
        let session_dir = std::env::var_os("UNPEEL_SESSION_ID")
            .filter(|id| !id.is_empty())
            .and_then(|_| std::env::var_os("UNPEEL_SESSION_DIR"))
            .map(PathBuf::from)
            .filter(|path| path.is_dir());
        Self {
            session_dir,
            last_context: None,
        }
    }

    pub fn publish(&mut self, cwd: &Path, selected: Option<&ExplorerEntry>) {
        let Some(directory) = &self.session_dir else {
            return;
        };
        let selected_path = selected.map(|entry| entry.path().to_path_buf());
        let next_context = (cwd.to_path_buf(), selected_path.clone());
        if self.last_context.as_ref() == Some(&next_context) {
            return;
        }
        self.last_context = Some(next_context);

        let context = json!({
            "cwd": cwd,
            "selected_path": selected_path,
            "selected_kind": selected.map(|entry| if entry.is_directory() { "directory" } else { "file" }),
        });
        let body = json!({
            "app": APP_ID,
            "context": context,
            "updated_at": now_ms(),
        });
        let temporary = directory.join(format!(".app-context.json.{}.tmp", std::process::id()));
        if std::fs::write(&temporary, body.to_string()).is_ok() {
            let _ = std::fs::rename(temporary, directory.join("app-context.json"));
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
