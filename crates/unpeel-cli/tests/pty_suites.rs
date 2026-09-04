//! Bridge from `cargo test` to the end-to-end suites in `tests/run.sh`.
//!
//! Those cases drive the real binary through a real PTY: they spawn hosted
//! processes, bind ports, and take a couple of minutes. That is too heavy to
//! run on every `cargo test`, so this is opt-in:
//!
//! ```sh
//! UNPEEL_TUI_PTY_TESTS=1 cargo test -p unpeel-cli --test pty_suites
//! # or, equivalently and with nicer output:
//! crates/unpeel-cli/tests/run.sh
//! ```
//!
//! Without the variable the test passes with a note, so a plain `cargo test`
//! stays fast and CI can opt in deliberately.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn pty_suites() {
    if std::env::var("UNPEEL_TUI_PTY_TESTS").is_err() {
        eprintln!(
            "skipping the PTY suites — set UNPEEL_TUI_PTY_TESTS=1 (or run \
             crates/unpeel-cli/tests/run.sh) to include them"
        );
        return;
    }
    let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("run.sh");
    assert!(runner.exists(), "missing {}", runner.display());

    let status = Command::new("bash")
        .arg(&runner)
        // cargo has already built the binary this test links against.
        .env("UNPEEL_TUI_SKIP_BUILD", "1")
        .status()
        .expect("failed to run the PTY suites");
    assert!(status.success(), "PTY suites failed — see the output above");
}
