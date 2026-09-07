#![cfg(all(unix, feature = "ui-bridge"))]

use std::process::Command;

use tempfile::tempdir;
use unpeel_app_kit::{AppMetadata, UI_SOCKET_ENV, UI_TOKEN_ENV, UiBridge};

const PROBE_ENV: &str = "UNPEEL_UI_ENV_SCRUB_PROBE";

#[test]
fn ui_bridge_detect_scrubs_credentials_before_spawning_children() {
    if std::env::var_os(PROBE_ENV).is_some() {
        let bridge = UiBridge::detect(AppMetadata::new("test.scrub", "Scrub probe", "1"))
            .expect("probe bridge should bind its inherited socket");
        assert!(bridge.is_available());
        assert!(std::env::var_os(UI_SOCKET_ENV).is_none());
        assert!(std::env::var_os(UI_TOKEN_ENV).is_none());

        let output = Command::new("env")
            .output()
            .expect("probe should be able to inspect a child environment");
        assert!(output.status.success());
        let environment = String::from_utf8(output.stdout).expect("environment must be UTF-8");
        assert!(!environment.lines().any(|line| {
            line.starts_with(&format!("{UI_SOCKET_ENV}="))
                || line.starts_with(&format!("{UI_TOKEN_ENV}="))
        }));
        return;
    }

    let directory = tempdir().expect("temporary socket directory");
    let executable = std::env::current_exe().expect("current integration test executable");
    let status = Command::new(executable)
        .arg("--exact")
        .arg("ui_bridge_detect_scrubs_credentials_before_spawning_children")
        .arg("--nocapture")
        .env(PROBE_ENV, "1")
        .env(UI_SOCKET_ENV, directory.path().join("scrub.sock"))
        .env(UI_TOKEN_ENV, "0123456789abcdef0123456789abcdef")
        .status()
        .expect("credential scrub probe should run");
    assert!(status.success());
}
