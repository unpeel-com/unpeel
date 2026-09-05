//! Unpeel session backend.
//!
//! Everything here is frontend-agnostic: it is consumed by the standalone
//! `unpeel-host` binary (session host + Unpeel Sessions MCP) that the native
//! Swift app (the `unpeel-apple` repository) spawns. No GUI/Tauri dependency may be added to
//! this crate — keeping it that way is what lets the host run headless.

// The unified MCP tool definitions are single large `json!` literals (one
// schema per domain); the computer tool's parameter set exceeds the default
// macro recursion limit.
#![recursion_limit = "256"]

#[cfg(feature = "native-host")]
pub mod activity_log;
#[cfg(feature = "native-host")]
pub mod app_installer;
#[cfg(feature = "native-host")]
pub mod app_open;
#[cfg(feature = "native-host")]
pub mod app_paths;
#[cfg(feature = "native-host")]
pub mod app_presentations;
#[cfg(feature = "native-host")]
pub mod app_runtime;
#[cfg(feature = "native-host")]
pub mod app_state;
#[cfg(feature = "native-host")]
pub mod apps_mcp;
#[cfg(feature = "native-host")]
pub mod browser_engine;
#[cfg(feature = "native-host")]
pub mod browser_mcp;
#[cfg(feature = "native-host")]
pub mod computer_engine;
#[cfg(feature = "native-host")]
pub mod computer_mcp;
#[cfg(feature = "native-host")]
pub mod controller_api;
#[cfg(feature = "native-host")]
pub mod controller_host;
#[cfg(feature = "controller-core")]
pub mod controller_protocol;
#[cfg(feature = "native-host")]
pub mod direct_connection;
#[cfg(feature = "controller-core")]
pub mod direct_path;
#[cfg(feature = "native-host")]
pub mod direct_path_client;
#[cfg(feature = "native-host")]
pub mod direct_path_punch;
#[cfg(feature = "native-host")]
pub mod first_run;
#[cfg(feature = "native-host")]
mod ghostty_vt;
#[cfg(feature = "native-host")]
pub mod hook_assets;
#[cfg(feature = "controller-core")]
pub mod host_connection;
#[cfg(feature = "native-host")]
pub mod http_fetch;
#[cfg(feature = "native-host")]
pub mod integrations;
#[cfg(feature = "native-host")]
pub mod license;
#[cfg(feature = "native-host")]
pub mod local_urls;
#[cfg(feature = "native-host")]
pub mod mcp_auth;
#[cfg(feature = "native-host")]
mod mcp_cancel;
#[cfg(feature = "native-host")]
pub mod mcp_gate;
#[cfg(feature = "native-host")]
pub mod mcp_host;
#[cfg(feature = "native-host")]
pub mod menu_prompt;
#[cfg(feature = "native-host")]
mod pane_context;
#[cfg(feature = "controller-core")]
#[cfg(feature = "native-host")]
pub mod pty_core;

pub mod relay_connection;
#[cfg(feature = "native-host")]
pub mod relay_crypto;
#[cfg(feature = "native-host")]
pub mod relay_downlink;
#[cfg(feature = "native-host")]
pub mod relay_probe;
#[cfg(feature = "native-host")]
pub mod relay_uplink;
#[cfg(feature = "controller-core")]
pub mod relay_wire;
#[cfg(feature = "native-host")]
pub mod remote_attach;
#[cfg(feature = "native-host")]
pub mod remote_server;
/// The TLS stack behind the Host's pinned certificate. Re-exported so the
/// serving crates and their tests use this one rustls, never a second copy.
#[cfg(feature = "native-host")]
pub use rustls;
#[cfg(feature = "controller-core")]
pub mod remote_session_backend;
#[cfg(feature = "native-host")]
pub mod remote_stdio;
#[cfg(feature = "native-host")]
pub mod resume;
#[cfg(feature = "native-host")]
pub mod runtime_catalog;
#[cfg(feature = "native-host")]
pub mod runtime_observer;
#[cfg(feature = "native-host")]
pub mod session_artifacts;
#[cfg(feature = "native-host")]
pub mod session_host;
#[cfg(feature = "native-host")]
pub mod session_input;
#[cfg(feature = "native-host")]
pub mod session_ops;
#[cfg(feature = "native-host")]
pub mod setup;
#[cfg(feature = "native-host")]
pub mod skills_mcp;
#[cfg(feature = "native-host")]
pub mod ssh_connection;
#[cfg(feature = "native-host")]
pub mod state;
#[cfg(feature = "native-host")]
pub mod state_bus;
#[cfg(feature = "native-host")]
pub mod terminal_viewport;
#[cfg(feature = "native-host")]
pub mod transcripts;
#[cfg(feature = "native-host")]
pub mod worktrees;
