use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

struct TempHome(PathBuf);

impl TempHome {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("unpeel-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .args(args)
        .env("UNPEEL_HOME", home)
        .output()
        .expect("run unpeel")
}

fn load_state(home: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(home.join("app-state.json")).unwrap()).unwrap()
}

#[test]
fn settings_and_preset_verbs_use_locked_unknown_preserving_state() {
    let home = TempHome::new();
    let state_path = home.path().join("app-state.json");
    let initial = serde_json::json!({
        "projects": [],
        "presets": [
            {
                "id": "alpha-id",
                "label": "Alpha",
                "command": "alpha",
                "enabled": true,
                "quick_launch": false,
                "future_preset_field": { "keep": 1 }
            },
            {
                "id": "beta-id",
                "label": "Beta",
                "command": "beta",
                "enabled": true,
                "quick_launch": false
            },
            {
                "id": "gamma-id",
                "label": "Gamma",
                "command": "gamma",
                "enabled": true,
                "quick_launch": false
            }
        ],
        "native_preset_overlay_migrated": true,
        "experimental_features": {
            "sessions_mcp": true,
            "browser_mcp": true,
            "future_gate": "keep"
        },
        "browser_default_access": "ask",
        "mcp_nonchild_write_access": "deny",
        "theme": "dark",
        "future_top_level": { "keep": true }
    });
    std::fs::write(&state_path, serde_json::to_vec_pretty(&initial).unwrap()).unwrap();

    let help = run(home.path(), &["settings", "--help"]);
    assert!(help.status.success(), "{help:?}");
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("experimental_features.sessions_mcp"));
    assert!(help.contains("captured when a Session launches"));

    let listed = run(home.path(), &["settings", "list", "--json"]);
    assert!(listed.status.success(), "{listed:?}");
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["experimental_features.sessions_mcp"], true);
    assert_eq!(listed["experimental_features.browser_mcp"], true);
    // Off until set — the same default as the Rust launch gate.
    assert_eq!(listed["experimental_features.computer_use"], false);
    assert_eq!(listed["browser_default_access"], "ask");
    assert_eq!(listed["mcp_nonchild_write_access"], "deny");
    assert_eq!(listed["theme"], "dark");

    // app_state::edit announces asynchronously, and the one-shot CLI must
    // flush before exiting so this request is already queued when output()
    // returns.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener
        .set_nonblocking(false)
        .expect("configure state-bus listener");
    std::fs::write(
        home.path().join("app-ports"),
        format!("{}\n", listener.local_addr().unwrap().port()),
    )
    .unwrap();
    let updated = run(
        home.path(),
        &[
            "settings",
            "set",
            "experimental_features.sessions_mcp",
            "false",
            "--json",
        ],
    );
    assert!(updated.status.success(), "{updated:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&updated.stdout).unwrap(),
        serde_json::json!({
            "key": "experimental_features.sessions_mcp",
            "value": false,
        })
    );
    listener.set_nonblocking(true).unwrap();
    let (mut stream, _) = listener
        .accept()
        .expect("state bus ping was flushed before exit");
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut request = String::new();
    stream.read_to_string(&mut request).unwrap();
    assert!(request.contains("POST /state-changed"), "{request:?}");
    assert!(request.contains("\"change\":\"app-state\""), "{request:?}");
    let _ = std::fs::remove_file(home.path().join("app-ports"));

    let state = load_state(home.path());
    assert_eq!(state["experimental_features"]["sessions_mcp"], false);
    assert_eq!(state["experimental_features"]["browser_mcp"], true);
    assert_eq!(state["experimental_features"]["future_gate"], "keep");
    assert_eq!(state["future_top_level"]["keep"], true);

    let before_invalid = std::fs::read(&state_path).unwrap();
    let invalid = run(
        home.path(),
        &["settings", "set", "browser_default_access", "allow"],
    );
    assert_eq!(invalid.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("expected on, ask, or off"));
    assert_eq!(std::fs::read(&state_path).unwrap(), before_invalid);

    for (key, value) in [
        ("experimental_features.browser_mcp", "false"),
        ("experimental_features.computer_use", "true"),
        ("browser_default_access", "off"),
        ("mcp_nonchild_write_access", "allow"),
        ("computer_access", "off"),
        ("mcp_worktree_access", "true"),
        ("mcp_auto_add_browser_screenshots", "false"),
        ("auto_stop_archive_minutes", "60"),
        ("sidebar_stopped_limit", "10"),
        ("theme", "light"),
    ] {
        let result = run(home.path(), &["settings", "set", key, value]);
        assert!(result.status.success(), "key={key} output={result:?}");
    }
    // The workspace keys land under the Host's own spellings and the
    // legacy `computer_access` key can no longer shadow the Host's.
    let state = load_state(home.path());
    assert_eq!(state["experimental_features"]["computer_use"], true);
    assert_eq!(state["experimental_features"]["future_gate"], "keep");
    assert_eq!(state["computer_default_access"], "off");
    assert!(state.get("computer_access").is_none());
    assert_eq!(state["mcp_worktree_access"], true);
    assert_eq!(state["mcp_auto_add_browser_screenshots"], false);
    assert_eq!(state["auto_stop_archive_minutes"], 60);
    assert_eq!(state["sidebar_stopped_limit"], 10);
    for (key, value) in [
        ("auto_stop_archive_minutes", "45"),
        ("sidebar_stopped_limit", "4"),
    ] {
        let rejected = run(home.path(), &["settings", "set", key, value]);
        assert_eq!(rejected.status.code(), Some(1), "key={key}");
    }
    let listed = run(home.path(), &["settings", "list", "--json"]);
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["computer_access"], "off");
    assert_eq!(listed["auto_stop_archive_minutes"], 60);

    let read_theme = run(home.path(), &["settings", "get", "theme", "--json"]);
    assert!(read_theme.status.success(), "{read_theme:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&read_theme.stdout).unwrap(),
        serde_json::json!({ "key": "theme", "value": "light" })
    );

    for args in [
        ["presets", "star", "beta-id"].as_slice(),
        ["presets", "disable", "Alpha"].as_slice(),
        ["presets", "reorder", "Gamma", "1"].as_slice(),
        ["presets", "unstar", "Beta"].as_slice(),
        ["presets", "enable", "alpha-id"].as_slice(),
    ] {
        let result = run(home.path(), args);
        assert!(result.status.success(), "args={args:?} output={result:?}");
    }

    let state = load_state(home.path());
    let presets = state["presets"].as_array().unwrap();
    let ids: Vec<&str> = presets
        .iter()
        .map(|preset| preset["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gamma-id", "alpha-id", "beta-id"]);
    assert_eq!(presets[1]["enabled"], true);
    assert_eq!(presets[2]["quick_launch"], false);
    assert_eq!(presets[1]["future_preset_field"]["keep"], 1);
    assert_eq!(state["future_top_level"]["keep"], true);
}
