use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn apps_install_refuses_noninteractive_without_yes() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home =
        std::env::temp_dir().join(format!("unpeel-apps-cli-{}-{nonce:x}", std::process::id()));

    let output = Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .args(["apps", "install", "unpeel.app.markdown"])
        .env("UNPEEL_HOME", &home)
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Refusing to install Markdown non-interactively"));
    assert!(stderr.contains("--yes"));
    assert!(!home.join("apps/bin/unpeel-markdown").exists());
    let _ = std::fs::remove_dir_all(home);
}
