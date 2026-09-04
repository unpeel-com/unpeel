use portable_pty::CommandBuilder;

pub mod shared;

use crate::session_host::SessionHostLaunch;

const APP_ACCENT_ENV: &str = "UNPEEL_APP_ACCENT";

type ConfigureHostCommand =
    fn(&SessionHostLaunch, &mut CommandBuilder, &mut Vec<String>) -> Result<(), String>;
type PrepareStartupCommand = fn(&str, RuntimeLaunchOptions) -> String;
type HasAutomaticMcpSetup = fn(&str) -> bool;
type PrepareRuntimeLaunch = fn(RuntimeLaunchOptions) -> Result<(), String>;
type LegacyMcpGateKind = fn(&str) -> Option<&'static str>;
type LegacyMcpGateGranted = fn(&str) -> bool;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeLaunchOptions {
    pub sessions_mcp: bool,
    pub browser_mcp: bool,
    pub computer_mcp: bool,
}

impl RuntimeLaunchOptions {
    pub fn any_mcp(self) -> bool {
        self.sessions_mcp || self.browser_mcp || self.computer_mcp
    }
}

#[derive(Clone, Copy)]
pub struct BuiltinPresetDefinition {
    pub id: &'static str,
    pub label: &'static str,
    pub command: &'static str,
    pub quick_launch: bool,
}

#[derive(Clone, Copy)]
pub struct Integration {
    pub install_runtime_support: Option<fn() -> Result<(), String>>,
    pub configure_host_command: Option<ConfigureHostCommand>,
    pub prepare_startup_command: Option<PrepareStartupCommand>,
    pub has_automatic_mcp_setup: Option<HasAutomaticMcpSetup>,
    pub prepare_runtime_launch: Option<PrepareRuntimeLaunch>,
    pub resume_adapter: Option<crate::resume::ResumeAdapter>,
    pub legacy_mcp_gate_kind: Option<LegacyMcpGateKind>,
    pub legacy_mcp_gate_granted: Option<LegacyMcpGateGranted>,
}

impl Integration {
    pub const fn new(
        install_runtime_support: Option<fn() -> Result<(), String>>,
        configure_host_command: Option<ConfigureHostCommand>,
    ) -> Self {
        Self {
            install_runtime_support,
            configure_host_command,
            prepare_startup_command: None,
            has_automatic_mcp_setup: None,
            prepare_runtime_launch: None,
            resume_adapter: None,
            legacy_mcp_gate_kind: None,
            legacy_mcp_gate_granted: None,
        }
    }

    pub const fn with_startup_command(
        mut self,
        prepare_startup_command: PrepareStartupCommand,
    ) -> Self {
        self.prepare_startup_command = Some(prepare_startup_command);
        self
    }

    pub const fn with_automatic_mcp_setup(
        mut self,
        has_automatic_mcp_setup: HasAutomaticMcpSetup,
    ) -> Self {
        self.has_automatic_mcp_setup = Some(has_automatic_mcp_setup);
        self
    }

    pub const fn with_runtime_launch_preparation(
        mut self,
        prepare_runtime_launch: PrepareRuntimeLaunch,
    ) -> Self {
        self.prepare_runtime_launch = Some(prepare_runtime_launch);
        self
    }

    pub const fn with_resume_adapter(
        mut self,
        resume_adapter: crate::resume::ResumeAdapter,
    ) -> Self {
        self.resume_adapter = Some(resume_adapter);
        self
    }

    pub const fn with_legacy_mcp_gate_kind(
        mut self,
        legacy_mcp_gate_kind: LegacyMcpGateKind,
    ) -> Self {
        self.legacy_mcp_gate_kind = Some(legacy_mcp_gate_kind);
        self
    }

    pub const fn with_legacy_mcp_gate_grant(
        mut self,
        legacy_mcp_gate_granted: LegacyMcpGateGranted,
    ) -> Self {
        self.legacy_mcp_gate_granted = Some(legacy_mcp_gate_granted);
        self
    }
}

include!(concat!(
    env!("OUT_DIR"),
    "/integration_adapters_generated.rs"
));

/// Resolve an argv alias owned by a runtime adapter into the provider-neutral
/// MCP gate kind. This exists only for persisted configs from older releases.
pub fn legacy_mcp_gate_kind(argument: &str) -> Option<&'static str> {
    INTEGRATIONS.iter().find_map(|(_, integration)| {
        integration
            .legacy_mcp_gate_kind
            .and_then(|resolve| resolve(argument))
    })
}

/// Ask runtime adapters whether one of their pre-migration environment aliases
/// grants this MCP domain. The shared gate still enforces Session identity.
pub fn legacy_mcp_gate_granted(kind: &str) -> bool {
    INTEGRATIONS.iter().any(|(_, integration)| {
        integration
            .legacy_mcp_gate_granted
            .is_some_and(|granted| granted(kind))
    })
}

pub(crate) fn integration_for_id(tool: &str) -> Option<&'static Integration> {
    let normalized = tool.trim();
    INTEGRATIONS
        .iter()
        .find(|(legacy_slug, _)| legacy_slug.eq_ignore_ascii_case(normalized))
        .map(|(_, integration)| integration)
}

pub(crate) fn runtime_for_command(
    command: &str,
) -> Option<&'static crate::runtime_catalog::RuntimeDescriptor> {
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_command_alias_for_current_platform(command_head(command))
}

pub(crate) fn integration_for_command(command: &str) -> Option<&'static Integration> {
    let runtime = runtime_for_command(command)?;
    integration_for_id(&runtime.legacy_slug)
}

fn runtime_for_dispatch(
    runtime_or_command: &str,
) -> Option<&'static crate::runtime_catalog::RuntimeDescriptor> {
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_legacy_slug_for_current_platform(runtime_or_command)
        .or_else(|| runtime_for_command(runtime_or_command))
}

fn integration_for_dispatch(runtime_or_command: &str) -> Option<&'static Integration> {
    let runtime = runtime_for_dispatch(runtime_or_command)?;
    integration_for_id(&runtime.legacy_slug)
}

pub fn command_head(command: &str) -> &str {
    shared::command_head(command)
}

pub fn builtin_presets() -> &'static [BuiltinPresetDefinition] {
    BUILTIN_PRESETS
}

pub fn preset_supports_quick_launch(command: &str) -> bool {
    runtime_for_command(command)
        .map(|runtime| runtime.supports_quick_launch)
        .unwrap_or(false)
}

pub fn uses_hook_port(tool: &str) -> bool {
    runtime_for_dispatch(tool)
        .map(|runtime| runtime.lifecycle.uses_hook_port())
        .unwrap_or(false)
}

pub fn has_runtime_support_installer(tool: &str) -> bool {
    integration_for_dispatch(tool)
        .is_some_and(|integration| integration.install_runtime_support.is_some())
}

pub fn install_runtime_support(tool: &str) -> Result<(), String> {
    let Some(integration) = integration_for_dispatch(tool) else {
        return Ok(());
    };

    if let Some(install) = integration.install_runtime_support {
        install()
    } else {
        Ok(())
    }
}

pub fn prepare_runtime_launch(
    tool: &str,
    mcp_enabled: bool,
    browser_mcp_enabled: bool,
    computer_mcp_enabled: bool,
) -> Result<(), String> {
    let options = RuntimeLaunchOptions {
        sessions_mcp: mcp_enabled,
        browser_mcp: browser_mcp_enabled,
        computer_mcp: computer_mcp_enabled,
    };
    if let Some(prepare) =
        integration_for_dispatch(tool).and_then(|integration| integration.prepare_runtime_launch)
    {
        prepare(options)?;
    }
    Ok(())
}

pub fn configure_host_command(
    tool: &str,
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    // The unified MCP host uses this identity for every domain. These paths
    // must also be explicit for hookless headless/CLI launches: otherwise an
    // isolated `UNPEEL_HOME` can fall back to the user's default ~/.unpeel.
    let session_dir = crate::app_paths::app_sessions_root().join(&launch.session.id);
    let session_dir_value = session_dir.to_string_lossy().to_string();
    let home = crate::app_paths::unpeel_home();
    let registry_value = home.join("app-ports").to_string_lossy().to_string();
    let trace_value = home
        .join("hooks")
        .join("trace.log")
        .to_string_lossy()
        .to_string();
    cmd.env("UNPEEL_SESSION_ID", &launch.session.id);
    cmd.env("UNPEEL_SESSION_DIR", &session_dir_value);
    cmd.env("UNPEEL_APP_PORT_REGISTRY_FILE", &registry_value);
    cmd.env("UNPEEL_HOOK_TRACE_FILE", &trace_value);
    shell_prelude.push(format!(
        "export UNPEEL_SESSION_ID={} UNPEEL_SESSION_DIR={} UNPEEL_APP_PORT_REGISTRY_FILE={} UNPEEL_HOOK_TRACE_FILE={}",
        shared::shell_quote(&launch.session.id),
        shared::shell_quote(&session_dir_value),
        shared::shell_quote(&registry_value),
        shared::shell_quote(&trace_value),
    ));

    // Frontends resolve the most local applicable color before launch:
    // project folder first, then workspace. Keep this an explicit hosted
    // environment value and clear any inherited parent-session accent when
    // the launch has none, so standalone/default styling remains honest.
    if let Some(accent) = launch.app_accent.as_deref().and_then(normalize_app_accent) {
        cmd.env(APP_ACCENT_ENV, &accent);
        shell_prelude.push(format!(
            "export {APP_ACCENT_ENV}={}",
            shared::shell_quote(&accent),
        ));
    } else {
        cmd.env_remove(APP_ACCENT_ENV);
        shell_prelude.push(format!("unset {APP_ACCENT_ENV}"));
    }

    if let Some(port) = launch.hook_port {
        let port_value = port.to_string();
        // Hook scripts persist the last lifecycle event into the session dir
        // (last-hook-event.json) so a restarted app can re-seed busy/attention
        // state. The shared paths above keep that state workspace-isolated.
        cmd.env("UNPEEL_APP_PORT", &port_value);
        shell_prelude.push(format!(
            "export UNPEEL_APP_PORT={}",
            shared::shell_quote(&port_value),
        ));
    } else {
        // `unpeel create` can itself run inside another hosted Session. Never
        // let the nested child inherit its parent's hook endpoint.
        cmd.env_remove("UNPEEL_APP_PORT");
        shell_prelude.push("unset UNPEEL_APP_PORT".to_string());
    }

    if let Some(configure) =
        integration_for_dispatch(tool).and_then(|integration| integration.configure_host_command)
    {
        configure(launch, cmd, shell_prelude)?;
    }

    Ok(())
}

fn normalize_app_accent(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", digits.to_ascii_uppercase()))
}

pub fn startup_command(
    tool: &str,
    command: &str,
    mcp_enabled: bool,
    browser_mcp_enabled: bool,
    computer_mcp_enabled: bool,
) -> String {
    let options = RuntimeLaunchOptions {
        sessions_mcp: mcp_enabled,
        browser_mcp: browser_mcp_enabled,
        computer_mcp: computer_mcp_enabled,
    };
    integration_for_dispatch(tool)
        .and_then(|integration| integration.prepare_startup_command)
        .map(|prepare| prepare(command, options))
        .unwrap_or_else(|| command.trim().to_string())
}

/// Whether this launch receives Unpeel's unified MCP client configuration
/// automatically. Domain authorization is recorded independently on the
/// Session manifest; this function is only provider-setup evidence.
///
/// Keep this aligned with `startup_command` plus each integration's
/// `configure_host_command`. A provider that merely runs in the PTY (or a
/// managed provider with no MCP integration) must remain false so clients do
/// not mistake a launch grant for completed provider configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutomaticMcpRegistration {
    pub sessions: bool,
    pub browser: bool,
    pub computer: bool,
}

pub fn automatic_mcp_registration(
    tool: &str,
    command: &str,
    mcp_enabled: bool,
    browser_mcp_enabled: bool,
    computer_mcp_enabled: bool,
) -> AutomaticMcpRegistration {
    if !(mcp_enabled || browser_mcp_enabled || computer_mcp_enabled) {
        return AutomaticMcpRegistration::default();
    }
    let Some(runtime) = runtime_for_dispatch(tool) else {
        return AutomaticMcpRegistration::default();
    };
    let unified = integration_for_id(&runtime.legacy_slug)
        .and_then(|integration| integration.has_automatic_mcp_setup)
        .is_some_and(|has_setup| has_setup(command));
    let supports = |capability| runtime.capabilities.contains(&capability);
    AutomaticMcpRegistration {
        sessions: unified
            && mcp_enabled
            && supports(crate::runtime_catalog::RuntimeCapability::McpSessions),
        browser: unified
            && browser_mcp_enabled
            && supports(crate::runtime_catalog::RuntimeCapability::McpBrowser),
        computer: unified
            && computer_mcp_enabled
            && supports(crate::runtime_catalog::RuntimeCapability::McpComputer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_mcp_gate_argv_is_resolved_by_its_runtime_adapter() {
        assert_eq!(
            legacy_mcp_gate_kind("__kiro_mcp__"),
            Some(crate::mcp_gate::UNIFIED_KIND)
        );
        assert_eq!(legacy_mcp_gate_kind("__unknown_mcp__"), None);
    }

    #[test]
    fn runtime_catalog_generated_registry_preserves_legacy_order_and_metadata() {
        let catalog = crate::runtime_catalog::builtin_runtime_catalog();
        let slugs = INTEGRATIONS
            .iter()
            .map(|(slug, _)| *slug)
            .collect::<Vec<_>>();
        let mut catalog_adapters = catalog
            .current_platform_descriptors()
            .filter(|runtime| {
                runtime
                    .adapter
                    .as_deref()
                    .is_some_and(|adapter| adapter.starts_with("builtin:"))
            })
            .collect::<Vec<_>>();
        catalog_adapters.sort_by_key(|runtime| runtime.legacy_order);
        assert_eq!(
            catalog_adapters
                .iter()
                .map(|runtime| runtime.legacy_slug.as_str())
                .collect::<Vec<_>>(),
            slugs,
            "every built-in descriptor must produce exactly one integration entry"
        );

        let mut catalog_preset_runtimes =
            catalog.current_platform_descriptors().collect::<Vec<_>>();
        catalog_preset_runtimes.sort_by(|left, right| {
            left.legacy_order
                .unwrap_or(u16::MAX)
                .cmp(&right.legacy_order.unwrap_or(u16::MAX))
                .then_with(|| left.slug.cmp(&right.slug))
                .then_with(|| left.id.cmp(&right.id))
        });
        let catalog_presets = catalog_preset_runtimes
            .iter()
            .flat_map(|runtime| runtime.suggested_presets.iter())
            .collect::<Vec<_>>();
        assert_eq!(catalog_presets.len(), BUILTIN_PRESETS.len());
        assert_eq!(catalog_presets.len(), BUILTIN_PRESET_IDS.len());
        for (catalog_preset, generated_preset) in catalog_presets.into_iter().zip(BUILTIN_PRESETS) {
            assert_eq!(catalog_preset.id, generated_preset.id);
            assert_eq!(catalog_preset.label, generated_preset.label);
            assert_eq!(catalog_preset.command, generated_preset.command);
            assert_eq!(catalog_preset.quick_launch, generated_preset.quick_launch);
        }
        for (preset_id, generated_preset) in BUILTIN_PRESET_IDS.iter().zip(BUILTIN_PRESETS) {
            assert_eq!(*preset_id, generated_preset.id);
        }

        for (legacy_slug, _) in INTEGRATIONS {
            let runtime = catalog
                .by_legacy_slug(legacy_slug)
                .unwrap_or_else(|| panic!("catalog missing {legacy_slug}"));
            assert!(runtime.supports_current_platform());
            assert_eq!(
                uses_hook_port(legacy_slug),
                runtime.lifecycle.uses_hook_port()
            );
            assert_eq!(
                preset_supports_quick_launch(legacy_slug),
                runtime.supports_quick_launch
            );

            let integration = integration_for_id(legacy_slug).expect("generated integration");
            assert_eq!(
                has_runtime_support_installer(legacy_slug),
                integration.install_runtime_support.is_some(),
                "{legacy_slug}: support-install dispatch must follow the adapter callback"
            );
            let declares_mcp = runtime.capabilities.iter().any(|capability| {
                matches!(
                    capability,
                    crate::runtime_catalog::RuntimeCapability::McpSessions
                        | crate::runtime_catalog::RuntimeCapability::McpBrowser
                        | crate::runtime_catalog::RuntimeCapability::McpComputer
                )
            });
            assert_eq!(
                integration.has_automatic_mcp_setup.is_some(),
                declares_mcp,
                "{legacy_slug}: MCP capabilities and automatic setup callback disagree"
            );
            if runtime.lifecycle.uses_hook_port() {
                assert!(
                    integration.install_runtime_support.is_some(),
                    "{legacy_slug}: hook-owned lifecycle needs an installer"
                );
            }
        }
    }

    #[test]
    fn command_dispatch_normalizes_absolute_paths_and_every_declared_alias() {
        let catalog = crate::runtime_catalog::builtin_runtime_catalog();
        for runtime in catalog
            .current_platform_descriptors()
            .filter(|runtime| runtime.adapter.is_some())
        {
            let expected = integration_for_id(&runtime.legacy_slug).expect("generated integration");
            for alias in &runtime.detection.command_aliases {
                for command in [
                    alias.clone(),
                    format!("/opt/unpeel/bin/{alias} --test-flag"),
                ] {
                    let actual = integration_for_command(&command)
                        .unwrap_or_else(|| panic!("missing dispatch for {command}"));
                    assert!(
                        std::ptr::eq(actual, expected),
                        "{command} did not dispatch through {}",
                        runtime.legacy_slug
                    );
                    assert_eq!(
                        runtime_for_command(&command).map(|value| value.id.as_str()),
                        Some(runtime.id.as_str())
                    );
                    assert_eq!(uses_hook_port(&command), runtime.lifecycle.uses_hook_port());
                    assert_eq!(
                        preset_supports_quick_launch(&command),
                        runtime.supports_quick_launch
                    );
                }
            }
        }

        assert!(integration_for_command("/opt/unpeel/bin/not-an-agent --flag").is_none());
        assert!(!has_runtime_support_installer(
            "/opt/unpeel/bin/not-an-agent"
        ));
        assert!(install_runtime_support("/opt/unpeel/bin/not-an-agent").is_ok());
        assert_eq!(
            startup_command(
                "/opt/unpeel/bin/not-an-agent",
                "/opt/unpeel/bin/not-an-agent --flag",
                true,
                true,
                true,
            ),
            "/opt/unpeel/bin/not-an-agent --flag"
        );
    }

    #[test]
    fn absolute_path_dispatch_reaches_runtime_startup_callback() {
        let command = startup_command(
            "/opt/unpeel/bin/cursor-agent",
            "/opt/unpeel/bin/cursor-agent --force",
            false,
            true,
            false,
        );
        assert_eq!(
            command,
            "/opt/unpeel/bin/cursor-agent --force --approve-mcps"
        );
    }

    #[test]
    fn runtime_catalog_automatic_mcp_registration_matches_setup_and_capabilities() {
        assert_eq!(
            automatic_mcp_registration("codex", "codex", true, false, true),
            AutomaticMcpRegistration {
                sessions: true,
                browser: false,
                computer: true,
            }
        );
        assert_eq!(
            automatic_mcp_registration("cline", "cline", false, true, false),
            AutomaticMcpRegistration {
                sessions: false,
                browser: true,
                computer: false,
            }
        );
        assert_eq!(
            automatic_mcp_registration("kiro-cli", "kiro-cli", true, true, true),
            AutomaticMcpRegistration {
                sessions: true,
                browser: true,
                computer: false,
            }
        );
        assert_eq!(
            automatic_mcp_registration("pi", "pi", true, true, true),
            AutomaticMcpRegistration::default()
        );
        assert_eq!(
            automatic_mcp_registration("cat", "cat", true, true, true),
            AutomaticMcpRegistration::default()
        );
        assert_eq!(
            automatic_mcp_registration(
                "claude",
                "claude --mcp-config /tmp/custom.json",
                true,
                false,
                false
            ),
            AutomaticMcpRegistration::default()
        );
        assert_eq!(
            automatic_mcp_registration("claude", "claude", false, false, false),
            AutomaticMcpRegistration::default()
        );
    }

    /// The hook env block must pin the registry/trace fallback paths to this
    /// instance's UNPEEL_HOME — otherwise a workspace instance's hook scripts
    /// broadcast against (and trace into) the real ~/.unpeel.
    #[test]
    fn hook_env_block_pins_registry_and_trace_to_unpeel_home() {
        let launch: SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "test-session",
                "project_id": "test-project",
                "label": "test",
                "command": "sh"
            },
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": 4321
        }))
        .expect("launch fixture");
        let mut cmd = CommandBuilder::new("true");
        let mut prelude = Vec::new();
        configure_host_command("sh", &launch, &mut cmd, &mut prelude).expect("configure");
        let exports = prelude.join("\n");
        let home = crate::app_paths::unpeel_home();
        let registry = shared::shell_quote(&home.join("app-ports").to_string_lossy());
        let trace = shared::shell_quote(&home.join("hooks").join("trace.log").to_string_lossy());
        assert!(
            exports.contains(&format!("UNPEEL_APP_PORT_REGISTRY_FILE={registry}")),
            "registry path missing from prelude: {exports}"
        );
        assert!(
            exports.contains(&format!("UNPEEL_HOOK_TRACE_FILE={trace}")),
            "trace path missing from prelude: {exports}"
        );
    }

    #[test]
    fn hosted_app_accent_is_normalized_and_exported() {
        let launch: SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "accent-session",
                "project_id": "test-project",
                "label": "test",
                "command": "sh"
            },
            "cwd": "/tmp",
            "dark_mode": true,
            "app_accent": " 4ec3c9 "
        }))
        .expect("launch fixture");
        let mut cmd = CommandBuilder::new("true");
        let mut prelude = Vec::new();

        configure_host_command("sh", &launch, &mut cmd, &mut prelude).expect("configure");

        assert_eq!(
            cmd.get_env(APP_ACCENT_ENV),
            Some(std::ffi::OsStr::new("#4EC3C9"))
        );
        assert!(prelude
            .join("\n")
            .contains("export UNPEEL_APP_ACCENT='#4EC3C9'"));
    }

    #[test]
    fn hookless_launch_keeps_mcp_identity_and_workspace_paths_without_parent_port() {
        let launch: SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "headless-session",
                "project_id": "test-project",
                "label": "test",
                "command": "sh"
            },
            "cwd": "/tmp",
            "dark_mode": null
        }))
        .expect("launch fixture");
        let mut cmd = CommandBuilder::new("true");
        cmd.env("UNPEEL_APP_PORT", "9999");
        cmd.env(APP_ACCENT_ENV, "#D97757");
        let mut prelude = Vec::new();

        configure_host_command("sh", &launch, &mut cmd, &mut prelude).expect("configure");

        let session_dir = crate::app_paths::app_sessions_root().join("headless-session");
        let home = crate::app_paths::unpeel_home();
        assert_eq!(
            cmd.get_env("UNPEEL_SESSION_ID"),
            Some(std::ffi::OsStr::new("headless-session"))
        );
        assert_eq!(
            cmd.get_env("UNPEEL_SESSION_DIR"),
            Some(session_dir.as_os_str())
        );
        assert_eq!(
            cmd.get_env("UNPEEL_APP_PORT_REGISTRY_FILE"),
            Some(home.join("app-ports").as_os_str())
        );
        assert_eq!(
            cmd.get_env("UNPEEL_HOOK_TRACE_FILE"),
            Some(home.join("hooks").join("trace.log").as_os_str())
        );
        assert_eq!(cmd.get_env("UNPEEL_APP_PORT"), None);
        assert_eq!(cmd.get_env(APP_ACCENT_ENV), None);

        let exports = prelude.join("\n");
        assert!(exports.contains("UNPEEL_SESSION_ID='headless-session'"));
        assert!(exports.contains("UNPEEL_SESSION_DIR="));
        assert!(exports.contains("UNPEEL_APP_PORT_REGISTRY_FILE="));
        assert!(exports.contains("UNPEEL_HOOK_TRACE_FILE="));
        assert!(exports.contains("unset UNPEEL_APP_PORT"));
        assert!(exports.contains("unset UNPEEL_APP_ACCENT"));
    }

    #[test]
    fn cursor_approves_the_unified_server_when_browser_is_the_only_domain() {
        let command = startup_command("cursor-agent", "cursor-agent --force", false, true, false);
        assert_eq!(command, "cursor-agent --force --approve-mcps");
    }
}
