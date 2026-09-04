//! Headless host serving stack.
//!
//! This is the UI-free half of what used to live inside `unpeel-cli`: the
//! app-less `/mobile/*` server, the relay uplink runtime, pairing, hook-event
//! ingestion, approvals, and the sidebar/session data model that feeds the
//! phone-facing snapshot. None of it depends on ratatui — the interactive TUI
//! is now one frontend on top of these modules. [`driver::HostRuntime`] is the
//! canonical per-workspace server core; the foreground `unpeel serve` command
//! is one runner, and the native app will migrate onto the same runtime with
//! platform capability adapters.
//!
//! The mutating control path (create/input/kill) already lives in
//! `unpeel_core::controller_api`; these modules are the read/publish side plus
//! HTTP/relay framing.

pub mod activity;
mod activity_snapshot;
mod app_context;
pub mod approvals;
pub mod auto_archive;
pub mod computer;
pub mod control;
pub mod direct_path;
pub mod driver;
pub mod hook_listener;
pub mod local_gateway;
pub mod mobile;
pub mod notifications;
pub mod overlay;
pub mod pairing;
pub mod platform_adapter;
pub mod presence;
pub mod pty_core_supervisor;
pub mod relay;
pub mod remote_streamer;
pub mod runtime_presentation;
pub mod service;
pub mod service_install;
pub mod sessions;
mod tracelog;

pub use driver::HostRuntime;
