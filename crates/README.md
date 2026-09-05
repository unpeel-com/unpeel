# crates/

The Rust workspace: Unpeel's session backend and terminal-side binaries. No
GUI dependency — everything here runs the same under the Mac app, the
terminal UI, or a headless Linux host. All of it is open per
the private "open-source" design record.

| crate | what it is |
| --- | --- |
| `unpeel-core` | The session backend library: hosted PTY sessions, provider integrations, hook assets, MCP host, browser MCP, transcripts, viewport |
| `unpeel-host` | The standalone host binary the clients spawn: session host, `unpeel` MCP server, remote server, transcript/viewport CLIs |
| `unpeel-cli` | The `unpeel` binary: terminal UI over the same hosted sessions, and the headless host on servers |
| `unpeel-native-bridge` | Panic-contained C ABI so the Swift Mac app can call unpeel-core directly |
| `unpeel-apps` | First-party Unpeel Apps (standalone-first CLIs) |

Build/test: `cargo test --manifest-path crates/Cargo.toml` (run after any
session-launch or hook change). The TUI additionally has a real-PTY
integration suite: `crates/unpeel-cli/tests/run.sh`.
