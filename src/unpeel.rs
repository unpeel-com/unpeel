use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::app::App;
use crate::install::APP_ID;

pub struct ContextReporter {
    session_dir: Option<PathBuf>,
    last_context: Option<String>,
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

    pub fn publish(&mut self, app: &App) {
        let Some(directory) = &self.session_dir else {
            return;
        };
        let context = json!({
            "root": app.root(),
            "view": if app.is_detail() { "diff" } else { "files" },
            "changed_files": app.files.len(),
            "selected_path": app.selected_absolute_path(),
            "selected_status": app.selected_file().map(|file| file.state_label()),
            "selected_diff_lines": app
                .selection_range()
                .map(|(start, end)| json!([start + 1, end + 1])),
        });
        let serialized = context.to_string();
        if self.last_context.as_deref() == Some(serialized.as_str()) {
            return;
        }
        self.last_context = Some(serialized);

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
