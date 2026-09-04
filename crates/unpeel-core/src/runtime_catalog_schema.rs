use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const RUNTIME_DESCRIPTOR_SCHEMA_VERSION: u32 = 1;
const MAX_RUNTIME_ICON_BYTES: u64 = 128 * 1024;
const MAX_WINDOW_PADDING_X: u8 = 48;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDescriptor {
    pub schema_version: u32,
    pub id: String,
    /// Source package slug and `runtimes/<slug>` directory name.
    pub slug: String,
    /// Compatibility identity used by current manifests, integrations, and
    /// Controller DTOs. This remains unchanged during the catalog migration.
    pub legacy_slug: String,
    pub label: String,
    pub adapter: Option<String>,
    /// Compatibility ordering for the compiled adapter/preset registry.
    /// Metadata-only runtimes without an adapter leave this unset.
    #[serde(default)]
    pub legacy_order: Option<u16>,
    pub platforms: Vec<RuntimePlatform>,
    #[serde(default)]
    pub supports_quick_launch: bool,
    pub display: RuntimeDisplay,
    #[serde(default)]
    pub install: Option<RuntimeInstall>,
    pub detection: RuntimeDetection,
    /// Inherited provider process identity that must not cross into a new
    /// hosted Session. The Host applies every built-in runtime's list at its
    /// generic PTY boundary before any managed command starts.
    #[serde(default)]
    pub environment: RuntimeEnvironment,
    pub lifecycle: RuntimeLifecycle,
    #[serde(default)]
    pub capabilities: Vec<RuntimeCapability>,
    /// Client-safe hints for ranking already-installed runtimes by their
    /// provider-owned local conversation stores. Transcript parsing remains
    /// adapter code; these entries only identify files to count.
    #[serde(default)]
    pub usage: RuntimeUsage,
    #[serde(default)]
    pub suggested_presets: Vec<RuntimeSuggestedPreset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePlatform {
    Macos,
    Linux,
}

impl RuntimePlatform {
    fn rust_target_os(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
        }
    }
}

/// Presentation family for sidebar logos and generic fallbacks. A new
/// runtime never needs a client enum case: unknown future kinds should be
/// added here and generated into clients, not guessed from the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    #[default]
    Agent,
    App,
    Editor,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDisplay {
    /// What this CLI is for presentation: an agent, an Unpeel App, an
    /// editor (for example a markdown CLI), or a plain terminal. Omitted
    /// descriptors stay `agent`.
    #[serde(default)]
    pub kind: RuntimeKind,
    #[serde(default)]
    pub tint: Option<String>,
    #[serde(default)]
    pub spinner_tint: Option<String>,
    #[serde(default = "default_icon")]
    pub icon: String,
    /// Package-relative SVG embedded into generated client metadata. The path
    /// must stay below this runtime's `assets/` directory.
    #[serde(default)]
    pub icon_asset: Option<String>,
    /// Provenance for a contributed icon: an https URL or an explicit
    /// `internal:` migration/generation marker.
    #[serde(default)]
    pub icon_source: Option<String>,
    /// SPDX identifier when applicable, otherwise a concise redistribution
    /// status such as `vendor-brand-asset`.
    #[serde(default)]
    pub icon_license: Option<String>,
    /// Template icons are recolored by the client. Set this to false only for
    /// an authored multi-color asset whose fills must be preserved.
    #[serde(default = "default_true")]
    pub icon_template: bool,
    /// Horizontal Ghostty window padding in points. Zero (the default) lets
    /// the runtime paint to the pane edge. Agent TUIs normally set a small
    /// inset; full-bleed runtimes can remain at zero.
    #[serde(default)]
    pub window_padding_x: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstall {
    pub official_url: String,
    #[serde(default)]
    pub command: Option<String>,
}

fn default_icon() -> String {
    "agent".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDetection {
    pub command_aliases: Vec<String>,
    pub process_aliases: Vec<String>,
    /// Runtime-owned executable locations relative to the user's home. Host
    /// setup may search these in addition to the platform defaults.
    #[serde(default)]
    pub search_path_suffixes: Vec<String>,
    /// All normalized path components in one inner set must occur for the
    /// package/script path to identify this runtime. Sets are alternatives.
    #[serde(default)]
    pub script_path_signatures: Vec<RuntimeScriptPathSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeScriptPathSignature {
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEnvironment {
    #[serde(default)]
    pub strip_inherited: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLifecycle {
    pub source: RuntimeLifecycleSource,
    pub authority: RuntimeLifecycleAuthority,
    pub fallback: RuntimeLifecycleFallback,
    pub completion_reliable: bool,
    pub attention_reliable: bool,
    /// Whether a persisted generic `Start` event may be anchored to recent
    /// terminal output when restoring the hook-owned lifecycle latch.
    #[serde(default = "default_true")]
    pub anchor_start_event_to_output: bool,
    /// Whether terminal output growth means an attention prompt was answered.
    #[serde(default = "default_true")]
    pub attention_clears_on_output: bool,
    /// Whether an idle/Stop hook is provisional while terminal output keeps
    /// growing (some runtimes emit Stops for internal sub-turns).
    #[serde(default)]
    pub distrust_stops_while_output_grows: bool,
}

impl RuntimeLifecycle {
    pub fn uses_hook_port(&self) -> bool {
        self.source == RuntimeLifecycleSource::Hooks
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecycleSource {
    Hooks,
    SelfReport,
    Screen,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecycleAuthority {
    Complete,
    Partial,
    IdentityOnly,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecycleFallback {
    Screen,
    Output,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCapability {
    LifecycleHooks,
    Resume,
    RestartAgent,
    McpSessions,
    McpBrowser,
    McpComputer,
    Transcript,
    NotifyWhenDone,
    /// The runtime writes meaningful, task-describing terminal titles
    /// (`OSC 0/2`) the Host may adopt as the live session label. Static
    /// branding/cwd titles do not qualify; leave this capability off.
    SemanticTerminalTitle,
}

impl RuntimeCapability {
    fn descriptor_name(self) -> &'static str {
        match self {
            Self::LifecycleHooks => "lifecycle_hooks",
            Self::Resume => "resume",
            Self::RestartAgent => "restart_agent",
            Self::McpSessions => "mcp_sessions",
            Self::McpBrowser => "mcp_browser",
            Self::McpComputer => "mcp_computer",
            Self::Transcript => "transcript",
            Self::NotifyWhenDone => "notify_when_done",
            Self::SemanticTerminalTitle => "semantic_terminal_title",
        }
    }

    fn requires_compiled_adapter(self) -> bool {
        match self {
            // Purely declarative: the Host's title scanner is provider-neutral.
            Self::Transcript | Self::SemanticTerminalTitle => false,
            Self::LifecycleHooks
            | Self::Resume
            | Self::RestartAgent
            | Self::McpSessions
            | Self::McpBrowser
            | Self::McpComputer
            | Self::NotifyWhenDone => true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeUsage {
    #[serde(default)]
    pub stores: Vec<RuntimeUsageStore>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeUsageStore {
    /// Provider store root relative to the user's home directory.
    pub root: String,
    pub extensions: Vec<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub file_name_suffix: Option<String>,
    #[serde(default)]
    pub parent_dir_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSuggestedPreset {
    pub id: String,
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub quick_launch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRuntimeDescriptor {
    pub directory_slug: String,
    pub source_path: PathBuf,
    pub descriptor: RuntimeDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCatalogError {
    messages: Vec<String>,
}

impl RuntimeCatalogError {
    fn single(message: impl Into<String>) -> Self {
        Self {
            messages: vec![message.into()],
        }
    }

    fn from_messages(messages: Vec<String>) -> Self {
        Self { messages }
    }

    #[allow(dead_code)] // Used by library consumers; the build-script copy only formats errors.
    pub fn messages(&self) -> &[String] {
        &self.messages
    }
}

impl fmt::Display for RuntimeCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, message) in self.messages.iter().enumerate() {
            if index != 0 {
                writeln!(formatter)?;
            }
            write!(formatter, "{message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RuntimeCatalogError {}

pub fn parse_runtime_descriptor(
    directory_slug: &str,
    source_path: impl Into<PathBuf>,
    contents: &str,
) -> Result<DiscoveredRuntimeDescriptor, RuntimeCatalogError> {
    let source_path = source_path.into();
    let descriptor = toml::from_str::<RuntimeDescriptor>(contents).map_err(|error| {
        RuntimeCatalogError::single(format!("{}: {error}", source_path.display()))
    })?;
    Ok(DiscoveredRuntimeDescriptor {
        directory_slug: directory_slug.to_string(),
        source_path,
        descriptor,
    })
}

pub fn discover_runtime_descriptors(
    root: &Path,
) -> Result<Vec<DiscoveredRuntimeDescriptor>, RuntimeCatalogError> {
    let entries = fs::read_dir(root).map_err(|error| {
        RuntimeCatalogError::single(format!(
            "failed to read runtime catalog {}: {error}",
            root.display()
        ))
    })?;
    let mut descriptor_paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            RuntimeCatalogError::single(format!(
                "failed to enumerate runtime catalog {}: {error}",
                root.display()
            ))
        })?;
        let path = entry.path();
        if path.is_dir() {
            let descriptor_path = path.join("runtime.toml");
            if descriptor_path.is_file() {
                descriptor_paths.push(descriptor_path);
            }
        }
    }
    descriptor_paths.sort();

    if descriptor_paths.is_empty() {
        return Err(RuntimeCatalogError::single(format!(
            "runtime catalog {} contains no runtime.toml descriptors",
            root.display()
        )));
    }

    let mut descriptors = Vec::with_capacity(descriptor_paths.len());
    for path in descriptor_paths {
        let directory_slug = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                RuntimeCatalogError::single(format!(
                    "runtime descriptor has a non-UTF-8 directory: {}",
                    path.display()
                ))
            })?;
        let contents = fs::read_to_string(&path).map_err(|error| {
            RuntimeCatalogError::single(format!("failed to read {}: {error}", path.display()))
        })?;
        descriptors.push(parse_runtime_descriptor(directory_slug, &path, &contents)?);
    }
    validate_runtime_descriptors(descriptors)
}

pub fn validate_runtime_descriptors(
    mut descriptors: Vec<DiscoveredRuntimeDescriptor>,
) -> Result<Vec<DiscoveredRuntimeDescriptor>, RuntimeCatalogError> {
    descriptors.sort_by(|left, right| {
        left.descriptor
            .slug
            .cmp(&right.descriptor.slug)
            .then_with(|| left.descriptor.id.cmp(&right.descriptor.id))
            .then_with(|| left.source_path.cmp(&right.source_path))
    });

    let mut errors = Vec::new();
    let mut ids = BTreeMap::<&str, &Path>::new();
    let mut package_slugs = BTreeMap::<&str, &Path>::new();
    let mut legacy_slugs = BTreeMap::<&str, &Path>::new();
    let mut aliases = BTreeMap::<&str, (&str, &Path)>::new();
    let mut preset_ids = BTreeMap::<&str, (&str, &Path)>::new();
    let mut legacy_orders = BTreeMap::<u16, (&str, &Path)>::new();
    let mut module_identifiers = BTreeMap::<String, (&str, &Path)>::new();
    let mut script_signatures = BTreeMap::<Vec<String>, (&str, &Path)>::new();

    for discovered in &descriptors {
        let descriptor = &discovered.descriptor;
        let source = discovered.source_path.as_path();
        let prefix = source.display().to_string();

        if descriptor.schema_version != RUNTIME_DESCRIPTOR_SCHEMA_VERSION {
            errors.push(format!(
                "{prefix}: unsupported schema_version {}; expected {}",
                descriptor.schema_version, RUNTIME_DESCRIPTOR_SCHEMA_VERSION
            ));
        }
        if !valid_stable_id(&descriptor.id) {
            errors.push(format!(
                "{prefix}: id '{}' must be a lowercase reverse-DNS identifier",
                descriptor.id
            ));
        }
        if !valid_slug(&descriptor.slug) {
            errors.push(format!(
                "{prefix}: slug '{}' is not a normalized runtime package slug",
                descriptor.slug
            ));
        }
        if !valid_slug(&descriptor.legacy_slug) {
            errors.push(format!(
                "{prefix}: legacy_slug '{}' is not a normalized runtime slug",
                descriptor.legacy_slug
            ));
        }
        if discovered.directory_slug != descriptor.slug {
            errors.push(format!(
                "{prefix}: directory '{}' must match slug '{}'",
                discovered.directory_slug, descriptor.slug
            ));
        }
        if descriptor.label.trim().is_empty() {
            errors.push(format!("{prefix}: label must not be empty"));
        }
        if descriptor.platforms.is_empty() {
            errors.push(format!("{prefix}: platforms must not be empty"));
        } else if has_duplicates(descriptor.platforms.iter().copied()) {
            errors.push(format!("{prefix}: platforms contains duplicate values"));
        }
        if descriptor
            .adapter
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push(format!("{prefix}: adapter must not be empty when present"));
        }
        match descriptor.adapter.as_deref() {
            Some(adapter) => {
                let expected = format!("builtin:{}", descriptor.legacy_slug);
                if adapter != expected {
                    errors.push(format!(
                        "{prefix}: built-in adapter must be '{expected}', got '{adapter}'"
                    ));
                }
                let module = rust_module_identifier(&descriptor.legacy_slug);
                if !valid_rust_module_identifier(&module) {
                    errors.push(format!(
                        "{prefix}: legacy_slug '{}' does not produce a valid Rust adapter module identifier",
                        descriptor.legacy_slug
                    ));
                } else if let Some((owner, previous)) =
                    module_identifiers.insert(module.clone(), (&descriptor.id, source))
                {
                    errors.push(format!(
                        "{prefix}: adapter module identifier '{module}' conflicts with runtime '{owner}' in {}",
                        previous.display()
                    ));
                }
                if let Some(order) = descriptor.legacy_order {
                    if let Some((owner, previous)) =
                        legacy_orders.insert(order, (&descriptor.id, source))
                    {
                        errors.push(format!(
                            "{prefix}: duplicate legacy_order {order} (runtime '{owner}' in {})",
                            previous.display()
                        ));
                    }
                } else {
                    errors.push(format!(
                        "{prefix}: a built-in adapter requires legacy_order"
                    ));
                }
            }
            None if descriptor.legacy_order.is_some() => errors.push(format!(
                "{prefix}: legacy_order is only valid with a built-in adapter"
            )),
            None => {}
        }
        validate_tint(
            &prefix,
            "display.tint",
            descriptor.display.tint.as_deref(),
            &mut errors,
        );
        validate_tint(
            &prefix,
            "display.spinner_tint",
            descriptor.display.spinner_tint.as_deref(),
            &mut errors,
        );
        if descriptor.display.icon.trim().is_empty() {
            errors.push(format!("{prefix}: display.icon must not be empty"));
        }
        if !descriptor.display.icon_template && descriptor.display.icon_asset.is_none() {
            errors.push(format!(
                "{prefix}: display.icon_template = false requires display.icon_asset"
            ));
        }
        if descriptor.display.window_padding_x > MAX_WINDOW_PADDING_X {
            errors.push(format!(
                "{prefix}: display.window_padding_x must be 0–{MAX_WINDOW_PADDING_X}, got {}",
                descriptor.display.window_padding_x
            ));
        }
        match (
            descriptor.display.icon_asset.as_deref(),
            descriptor.display.icon_source.as_deref(),
            descriptor.display.icon_license.as_deref(),
        ) {
            (Some(_), Some(source), Some(license)) => {
                if !(source.starts_with("https://") || source.starts_with("internal:"))
                    || source.chars().any(char::is_whitespace)
                {
                    errors.push(format!(
                        "{prefix}: display.icon_source must be an https URL or internal: marker without whitespace"
                    ));
                }
                if license.trim().is_empty() || license.trim() != license {
                    errors.push(format!(
                        "{prefix}: display.icon_license must be a non-empty trimmed value"
                    ));
                }
            }
            (Some(_), _, _) => errors.push(format!(
                "{prefix}: display.icon_asset requires display.icon_source and display.icon_license"
            )),
            (None, Some(_), _) | (None, _, Some(_)) => errors.push(format!(
                "{prefix}: display.icon_source and display.icon_license require display.icon_asset"
            )),
            (None, None, None) => {}
        }
        if let Some(icon_asset) = descriptor.display.icon_asset.as_deref() {
            if !valid_runtime_icon_asset_path(icon_asset) {
                errors.push(format!(
                    "{prefix}: display.icon_asset '{icon_asset}' must be a safe SVG path below assets/"
                ));
            } else if source.is_file() {
                validate_runtime_icon_asset(&prefix, source, icon_asset, &mut errors);
            }
        }
        if let Some(install) = &descriptor.install {
            if !install.official_url.starts_with("https://")
                || install.official_url.chars().any(char::is_whitespace)
            {
                errors.push(format!(
                    "{prefix}: install.official_url must be an https URL"
                ));
            }
            if install
                .command
                .as_deref()
                .is_some_and(|command| command.trim().is_empty())
            {
                errors.push(format!(
                    "{prefix}: install.command must not be empty when present"
                ));
            }
        }

        if let Some(previous) = ids.insert(&descriptor.id, source) {
            errors.push(format!(
                "{prefix}: duplicate runtime id '{}' (already declared by {})",
                descriptor.id,
                previous.display()
            ));
        }
        if let Some(previous) = package_slugs.insert(&descriptor.slug, source) {
            errors.push(format!(
                "{prefix}: duplicate package slug '{}' (already declared by {})",
                descriptor.slug,
                previous.display()
            ));
        }
        if let Some(previous) = legacy_slugs.insert(&descriptor.legacy_slug, source) {
            errors.push(format!(
                "{prefix}: duplicate legacy_slug '{}' (already declared by {})",
                descriptor.legacy_slug,
                previous.display()
            ));
        }

        if descriptor.detection.command_aliases.is_empty() {
            errors.push(format!(
                "{prefix}: detection.command_aliases must not be empty"
            ));
        }
        if !descriptor
            .detection
            .command_aliases
            .iter()
            .any(|alias| alias == &descriptor.legacy_slug)
        {
            errors.push(format!(
                "{prefix}: command aliases must include legacy_slug '{}'",
                descriptor.legacy_slug
            ));
        }
        let mut local_aliases = BTreeSet::new();
        for (kind, values) in [
            ("command", &descriptor.detection.command_aliases),
            ("process", &descriptor.detection.process_aliases),
        ] {
            let mut kind_aliases = BTreeSet::new();
            for alias in values {
                if !valid_alias(alias) {
                    errors.push(format!(
                        "{prefix}: {kind} alias '{alias}' must be a normalized executable basename"
                    ));
                    continue;
                }
                if !kind_aliases.insert(alias.as_str()) {
                    errors.push(format!("{prefix}: duplicate {kind} alias '{alias}'"));
                }
                local_aliases.insert(alias.as_str());
            }
        }
        for alias in local_aliases {
            if let Some((owner, previous)) = aliases.get(alias) {
                if *owner != descriptor.id {
                    errors.push(format!(
                        "{prefix}: alias '{alias}' conflicts with runtime '{owner}' in {}",
                        previous.display()
                    ));
                }
            } else {
                aliases.insert(alias, (&descriptor.id, source));
            }
        }

        if has_duplicates(descriptor.detection.search_path_suffixes.iter()) {
            errors.push(format!(
                "{prefix}: detection.search_path_suffixes contains duplicate values"
            ));
        }
        for suffix in &descriptor.detection.search_path_suffixes {
            let segments = suffix.split('/').collect::<Vec<_>>();
            if suffix.is_empty()
                || suffix.trim() != suffix
                || Path::new(suffix).is_absolute()
                || suffix.starts_with(['/', '\\'])
                || suffix.contains('\\')
                || segments
                    .iter()
                    .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
            {
                errors.push(format!(
                    "{prefix}: detection.search_path_suffix '{suffix}' must be a safe non-empty relative path"
                ));
            }
        }

        let mut component_sets = BTreeSet::new();
        for signature in &descriptor.detection.script_path_signatures {
            let components = &signature.components;
            if components.is_empty() {
                errors.push(format!(
                    "{prefix}: script_path_signatures cannot contain empty components"
                ));
                continue;
            }
            let mut normalized = components.clone();
            if normalized
                .iter()
                .any(|component| !valid_path_component(component))
            {
                errors.push(format!(
                    "{prefix}: script path components must be normalized executable/path basenames"
                ));
            }
            normalized.sort();
            if has_duplicates(normalized.iter()) {
                errors.push(format!(
                    "{prefix}: a script path component set contains duplicate components"
                ));
            }
            if !component_sets.insert(normalized) {
                errors.push(format!("{prefix}: duplicate script path component set"));
            }
        }
        for components in component_sets {
            if let Some((owner, previous)) =
                script_signatures.insert(components.clone(), (&descriptor.id, source))
            {
                errors.push(format!(
                    "{prefix}: script path component set {components:?} conflicts with runtime '{owner}' in {}",
                    previous.display()
                ));
            }
        }

        if has_duplicates(descriptor.environment.strip_inherited.iter()) {
            errors.push(format!(
                "{prefix}: environment.strip_inherited contains duplicate values"
            ));
        }
        for key in &descriptor.environment.strip_inherited {
            let mut characters = key.chars();
            let valid = characters
                .next()
                .is_some_and(|character| character == '_' || character.is_ascii_uppercase())
                && characters.all(|character| {
                    character == '_' || character.is_ascii_uppercase() || character.is_ascii_digit()
                });
            if !valid {
                errors.push(format!(
                    "{prefix}: environment.strip_inherited key '{key}' must be a normalized environment name"
                ));
            }
        }

        if has_duplicates(descriptor.capabilities.iter().copied()) {
            errors.push(format!("{prefix}: capabilities contains duplicate values"));
        }
        if descriptor.adapter.is_none() {
            let adapter_owned = descriptor
                .capabilities
                .iter()
                .copied()
                .filter(|capability| capability.requires_compiled_adapter())
                .map(RuntimeCapability::descriptor_name)
                .collect::<Vec<_>>();
            if !adapter_owned.is_empty() {
                errors.push(format!(
                    "{prefix}: adapter-owned capabilities [{}] require adapter = 'builtin:{}'",
                    adapter_owned.join(", "),
                    descriptor.legacy_slug
                ));
            }
        }
        let transcript_adapter_path = source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("adapter")
            .join("transcript.rs");
        let has_transcript_capability = descriptor
            .capabilities
            .contains(&RuntimeCapability::Transcript);
        let has_transcript_adapter = transcript_adapter_path.is_file();
        // Embedded descriptors use logical source paths and have no runtime
        // dependency on the source tree. Enforce package/file coupling while
        // discovering real on-disk packages (builds, tests, contributor tools).
        if source.is_file() && has_transcript_capability != has_transcript_adapter {
            errors.push(if has_transcript_capability {
                format!(
                    "{prefix}: transcript capability requires {}",
                    transcript_adapter_path.display()
                )
            } else {
                format!(
                    "{prefix}: {} requires the transcript capability",
                    transcript_adapter_path.display()
                )
            });
        }
        let has_hook_capability = descriptor
            .capabilities
            .contains(&RuntimeCapability::LifecycleHooks);
        if descriptor.lifecycle.uses_hook_port() != has_hook_capability {
            errors.push(format!(
                "{prefix}: lifecycle source and lifecycle_hooks capability disagree"
            ));
        }
        if descriptor.lifecycle.source == RuntimeLifecycleSource::Output
            && descriptor.lifecycle.authority != RuntimeLifecycleAuthority::None
        {
            errors.push(format!(
                "{prefix}: output lifecycle source must use authority = 'none'"
            ));
        }
        if descriptor.lifecycle.authority == RuntimeLifecycleAuthority::None
            && descriptor.lifecycle.fallback != RuntimeLifecycleFallback::None
        {
            errors.push(format!(
                "{prefix}: lifecycle authority = 'none' must use fallback = 'none'"
            ));
        }
        if descriptor.lifecycle.completion_reliable
            != descriptor
                .capabilities
                .contains(&RuntimeCapability::NotifyWhenDone)
        {
            errors.push(format!(
                "{prefix}: completion_reliable and notify_when_done capability disagree"
            ));
        }

        if has_duplicates(descriptor.usage.stores.iter()) {
            errors.push(format!("{prefix}: usage.stores contains duplicate values"));
        }
        for store in &descriptor.usage.stores {
            let segments = store.root.split('/').collect::<Vec<_>>();
            if store.root.is_empty()
                || store.root.trim() != store.root
                || Path::new(&store.root).is_absolute()
                || store.root.starts_with(['/', '\\'])
                || store.root.contains('\\')
                || segments
                    .iter()
                    .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
            {
                errors.push(format!(
                    "{prefix}: usage store root '{}' must be a safe non-empty home-relative path",
                    store.root
                ));
            }
            if store.extensions.is_empty()
                || has_duplicates(store.extensions.iter())
                || store.extensions.iter().any(|extension| {
                    extension.is_empty()
                        || extension.starts_with('.')
                        || !extension.chars().all(|character| {
                            character.is_ascii_lowercase() || character.is_ascii_digit()
                        })
                })
            {
                errors.push(format!(
                    "{prefix}: usage store '{}' needs unique normalized extensions without dots",
                    store.root
                ));
            }
            for (field, value) in [
                ("file_name", store.file_name.as_deref()),
                ("file_name_suffix", store.file_name_suffix.as_deref()),
                ("parent_dir_name", store.parent_dir_name.as_deref()),
            ] {
                if let Some(value) = value {
                    if value.is_empty()
                        || value.trim() != value
                        || value.contains(['/', '\\'])
                        || matches!(value, "." | "..")
                    {
                        errors.push(format!(
                            "{prefix}: usage store {field} '{value}' must be a safe basename"
                        ));
                    }
                }
            }
        }

        let mut local_preset_ids = BTreeSet::new();
        for preset in &descriptor.suggested_presets {
            if !valid_slug(&preset.id) {
                errors.push(format!(
                    "{prefix}: suggested preset id '{}' is not normalized",
                    preset.id
                ));
            }
            if !local_preset_ids.insert(preset.id.as_str()) {
                errors.push(format!(
                    "{prefix}: duplicate suggested preset id '{}'",
                    preset.id
                ));
            }
            if let Some((owner, previous)) = preset_ids.insert(&preset.id, (&descriptor.id, source))
            {
                if owner != descriptor.id {
                    errors.push(format!(
                        "{prefix}: suggested preset id '{}' conflicts with runtime '{owner}' in {}",
                        preset.id,
                        previous.display()
                    ));
                }
            }
            if preset.label.trim().is_empty() || preset.command.trim().is_empty() {
                errors.push(format!(
                    "{prefix}: suggested preset '{}' needs a label and command",
                    preset.id
                ));
            }
        }
    }

    if errors.is_empty() {
        Ok(descriptors)
    } else {
        errors.sort();
        Err(RuntimeCatalogError::from_messages(errors))
    }
}

#[allow(dead_code)] // Called by build.rs; the library copy keeps validation shared.
pub fn generated_catalog_source(descriptors: &[DiscoveredRuntimeDescriptor]) -> String {
    let mut slugs = descriptors
        .iter()
        .map(|runtime| runtime.descriptor.slug.as_str())
        .collect::<Vec<_>>();
    slugs.sort_unstable();
    let mut output = String::from(
        "// @generated by unpeel-core/build.rs; do not edit.\n\
         pub(crate) const BUILTIN_RUNTIME_DESCRIPTOR_TOML: &[(&str, &str)] = &[\n",
    );
    for slug in slugs {
        output.push_str(&format!(
            "    ({slug:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../runtimes/{slug}/runtime.toml\"))),\n"
        ));
    }
    output.push_str("];\n");
    output
}

#[allow(dead_code)] // Called by build.rs; the library copy keeps validation shared.
pub fn generated_integration_source(descriptors: &[DiscoveredRuntimeDescriptor]) -> String {
    let mut adapters = descriptors
        .iter()
        .filter(|runtime| runtime.descriptor.adapter.is_some())
        .collect::<Vec<_>>();
    adapters.sort_by_key(|runtime| runtime.descriptor.legacy_order.unwrap_or(u16::MAX));

    let mut output = String::from("// @generated by unpeel-core/build.rs; do not edit.\n");
    for runtime in &adapters {
        let descriptor = &runtime.descriptor;
        let module = rust_module_identifier(&descriptor.legacy_slug);
        let package = &descriptor.slug;
        output.push_str(&rust_platform_cfg_attribute(&descriptor.platforms));
        output.push_str(&format!(
            "pub(crate) mod {module} {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../runtimes/{package}/adapter/mod.rs\")); }}\n"
        ));
    }

    // Suggested presets are client-safe metadata, not adapter behavior. Keep
    // adapter-free runtimes in the Rust/TUI preset catalog just as the Swift
    // catalog does; only the compiled integration registry below is filtered
    // to runtimes that actually provide adapter code.
    let mut preset_runtimes = descriptors.iter().collect::<Vec<_>>();
    preset_runtimes.sort_by(|left, right| {
        left.descriptor
            .legacy_order
            .unwrap_or(u16::MAX)
            .cmp(&right.descriptor.legacy_order.unwrap_or(u16::MAX))
            .then_with(|| left.descriptor.slug.cmp(&right.descriptor.slug))
            .then_with(|| left.descriptor.id.cmp(&right.descriptor.id))
    });
    let presets = preset_runtimes
        .iter()
        .flat_map(|runtime| {
            runtime
                .descriptor
                .suggested_presets
                .iter()
                .map(move |preset| (*runtime, preset))
        })
        .collect::<Vec<_>>();
    output.push_str("\npub const BUILTIN_PRESET_IDS: &[&str] = &[\n");
    for (runtime, preset) in &presets {
        output.push_str(&rust_platform_cfg_attribute(&runtime.descriptor.platforms));
        output.push_str(&format!("    {:?},\n", preset.id));
    }
    output.push_str("];\n\nconst BUILTIN_PRESETS: &[BuiltinPresetDefinition] = &[\n");
    for (runtime, preset) in &presets {
        output.push_str(&rust_platform_cfg_attribute(&runtime.descriptor.platforms));
        output.push_str(&format!(
            "    BuiltinPresetDefinition {{ id: {:?}, label: {:?}, command: {:?}, quick_launch: {} }},\n",
            preset.id, preset.label, preset.command, preset.quick_launch
        ));
    }
    output.push_str("];\n\nconst INTEGRATIONS: &[(&str, Integration)] = &[\n");
    for runtime in adapters {
        let descriptor = &runtime.descriptor;
        let module = rust_module_identifier(&descriptor.legacy_slug);
        output.push_str(&rust_platform_cfg_attribute(&descriptor.platforms));
        output.push_str(&format!(
            "    ({:?}, {module}::INTEGRATION),\n",
            descriptor.legacy_slug
        ));
    }
    output.push_str("];\n");
    output
}

/// Generate provider transcript modules and their callback registry directly
/// from runtime packages. A transcript-capable package is therefore complete
/// when its descriptor and `adapter/transcript.rs` agree; no central provider
/// enum or match table needs editing.
#[allow(dead_code)] // Called by build.rs; the library copy keeps generation shared.
pub fn generated_transcript_source(descriptors: &[DiscoveredRuntimeDescriptor]) -> String {
    let mut adapters = descriptors
        .iter()
        .filter(|runtime| {
            runtime
                .descriptor
                .capabilities
                .contains(&RuntimeCapability::Transcript)
        })
        .collect::<Vec<_>>();
    adapters.sort_by(|left, right| left.descriptor.slug.cmp(&right.descriptor.slug));

    let mut output = String::from("// @generated by unpeel-core/build.rs; do not edit.\n");
    for runtime in &adapters {
        let module = format!(
            "transcript_{}",
            rust_module_identifier(&runtime.descriptor.slug)
        );
        let package = &runtime.descriptor.slug;
        output.push_str(&rust_platform_cfg_attribute(&runtime.descriptor.platforms));
        output.push_str(&format!(
            "mod {module} {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../runtimes/{package}/adapter/transcript.rs\")); }}\n"
        ));
    }
    output.push_str("\nconst TRANSCRIPT_ADAPTERS: &[&TranscriptAdapter] = &[\n");
    for runtime in adapters {
        let module = format!(
            "transcript_{}",
            rust_module_identifier(&runtime.descriptor.slug)
        );
        output.push_str(&rust_platform_cfg_attribute(&runtime.descriptor.platforms));
        output.push_str(&format!("    &{module}::ADAPTER,\n"));
    }
    output.push_str("];\n");
    output
}

#[allow(dead_code)]
fn rust_module_identifier(slug: &str) -> String {
    slug.replace('-', "_")
}

fn rust_platform_cfg_attribute(platforms: &[RuntimePlatform]) -> String {
    let mut targets = platforms
        .iter()
        .copied()
        .map(RuntimePlatform::rust_target_os)
        .collect::<Vec<_>>();
    targets.sort_unstable();
    targets.dedup();
    match targets.as_slice() {
        [target] => format!("#[cfg(target_os = {target:?})]\n"),
        targets => format!(
            "#[cfg(any({}))]\n",
            targets
                .iter()
                .map(|target| format!("target_os = {target:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn valid_rust_module_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !matches!(
            value,
            "as" | "break"
                | "const"
                | "continue"
                | "crate"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "async"
                | "await"
                | "dyn"
                | "abstract"
                | "become"
                | "box"
                | "do"
                | "final"
                | "macro"
                | "override"
                | "priv"
                | "typeof"
                | "unsized"
                | "virtual"
                | "yield"
                | "try"
        )
}

fn valid_stable_id(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() >= 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_alias(value: &str) -> bool {
    valid_slug(value) && !value.contains(['/', '\\']) && !value.chars().any(char::is_whitespace)
}

fn valid_path_component(value: &str) -> bool {
    let value = value.strip_prefix('@').unwrap_or(value);
    valid_slug(value) && !value.contains(['/', '\\']) && !value.chars().any(char::is_whitespace)
}

fn valid_runtime_icon_asset_path(value: &str) -> bool {
    let segments = value.split('/').collect::<Vec<_>>();
    value.trim() == value
        && !value.is_empty()
        && !Path::new(value).is_absolute()
        && !value.contains('\\')
        && segments.first() == Some(&"assets")
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && !matches!(*segment, "." | ".."))
        && Path::new(value)
            .extension()
            .and_then(|value| value.to_str())
            == Some("svg")
}

fn validate_runtime_icon_asset(
    prefix: &str,
    descriptor_path: &Path,
    relative_path: &str,
    errors: &mut Vec<String>,
) {
    let package_root = descriptor_path.parent().unwrap_or_else(|| Path::new("."));
    let asset_path = package_root.join(relative_path);
    let metadata = match fs::symlink_metadata(&asset_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            errors.push(format!(
                "{prefix}: display.icon_asset '{}' cannot be read: {error}",
                asset_path.display()
            ));
            return;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        errors.push(format!(
            "{prefix}: display.icon_asset '{}' must be a regular file, not a symlink",
            asset_path.display()
        ));
        return;
    }
    if metadata.len() > MAX_RUNTIME_ICON_BYTES {
        errors.push(format!(
            "{prefix}: display.icon_asset '{}' exceeds the {} byte limit",
            asset_path.display(),
            MAX_RUNTIME_ICON_BYTES
        ));
        return;
    }
    if let (Ok(canonical_root), Ok(canonical_asset)) = (
        fs::canonicalize(package_root),
        fs::canonicalize(&asset_path),
    ) {
        if !canonical_asset.starts_with(&canonical_root) {
            errors.push(format!(
                "{prefix}: display.icon_asset '{}' escapes its runtime package",
                asset_path.display()
            ));
            return;
        }
    }
    match fs::read_to_string(&asset_path) {
        Ok(contents) => {
            let contents = contents.trim_start_matches('\u{feff}').trim();
            if contents.is_empty() || !contents.contains("<svg") || !contents.contains("</svg>") {
                errors.push(format!(
                    "{prefix}: display.icon_asset '{}' must contain a complete UTF-8 SVG document",
                    asset_path.display()
                ));
            }
        }
        Err(error) => errors.push(format!(
            "{prefix}: display.icon_asset '{}' must be valid UTF-8: {error}",
            asset_path.display()
        )),
    }
}

fn validate_tint(prefix: &str, field: &str, value: Option<&str>, errors: &mut Vec<String>) {
    if let Some(value) = value {
        let valid = value.len() == 7
            && value.starts_with('#')
            && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit());
        if !valid {
            errors.push(format!("{prefix}: {field} '{value}' must be #RRGGBB"));
        }
    }
}

fn has_duplicates<T: Ord>(values: impl IntoIterator<Item = T>) -> bool {
    let mut seen = BTreeSet::new();
    values.into_iter().any(|value| !seen.insert(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn descriptor(slug: &str, id: &str) -> String {
        format!(
            r##"schema_version = 1
id = "{id}"
slug = "{slug}"
legacy_slug = "{slug}"
label = "Test"
adapter = "builtin:{slug}"
legacy_order = 1
platforms = ["macos", "linux"]
supports_quick_launch = true
capabilities = ["lifecycle_hooks", "resume", "restart_agent", "notify_when_done"]

[display]
tint = "#112233"
icon = "agent"

[install]
official_url = "https://example.com"

[detection]
command_aliases = ["{slug}"]
process_aliases = ["{slug}"]
script_path_signatures = []

[lifecycle]
source = "hooks"
authority = "complete"
fallback = "none"
completion_reliable = true
attention_reliable = true
"##
        )
    }

    fn parsed(slug: &str, id: &str) -> DiscoveredRuntimeDescriptor {
        parse_runtime_descriptor(slug, format!("/{slug}/runtime.toml"), &descriptor(slug, id))
            .unwrap()
    }

    #[test]
    fn lifecycle_controller_policy_has_compatible_defaults() {
        let descriptor = parsed("alpha", "com.example.alpha").descriptor;
        let lifecycle = descriptor.lifecycle;
        assert!(lifecycle.anchor_start_event_to_output);
        assert!(lifecycle.attention_clears_on_output);
        assert!(!lifecycle.distrust_stops_while_output_grows);
        assert!(descriptor.display.icon_template);
        assert!(descriptor.display.icon_asset.is_none());
        assert_eq!(descriptor.display.kind, RuntimeKind::Agent);
        assert_eq!(descriptor.display.window_padding_x, 0);
    }

    #[test]
    fn display_window_padding_x_accepts_inset_and_rejects_oversize() {
        let with_padding = descriptor("alpha", "com.example.alpha")
            .replace("icon = \"agent\"", "icon = \"agent\"\nwindow_padding_x = 8");
        let parsed =
            parse_runtime_descriptor("alpha", "/alpha/runtime.toml", &with_padding).unwrap();
        assert_eq!(parsed.descriptor.display.window_padding_x, 8);
        assert!(validate_runtime_descriptors(vec![parsed]).is_ok());

        let oversize = descriptor("alpha", "com.example.alpha").replace(
            "icon = \"agent\"",
            "icon = \"agent\"\nwindow_padding_x = 64",
        );
        let runtime = parse_runtime_descriptor("alpha", "/alpha/runtime.toml", &oversize).unwrap();
        let error = validate_runtime_descriptors(vec![runtime])
            .unwrap_err()
            .to_string();
        assert!(error.contains("display.window_padding_x"), "{error}");
    }

    #[test]
    fn display_kind_defaults_to_agent_and_accepts_editor() {
        let raw = descriptor("alpha", "com.example.alpha")
            .replace("icon = \"agent\"", "kind = \"editor\"\nicon = \"alpha\"");
        let parsed = parse_runtime_descriptor("alpha", "/alpha/runtime.toml", &raw).unwrap();
        assert_eq!(parsed.descriptor.display.kind, RuntimeKind::Editor);

        let bad = descriptor("alpha", "com.example.alpha")
            .replace("icon = \"agent\"", "kind = \"ide\"\nicon = \"alpha\"");
        let error = parse_runtime_descriptor("alpha", "/alpha/runtime.toml", &bad)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("kind") || error.contains("unknown variant"),
            "{error}"
        );
    }

    #[test]
    fn runtime_icon_assets_are_validated_inside_the_package() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("alpha");
        fs::create_dir_all(dir.join("assets")).unwrap();
        let source = dir.join("runtime.toml");
        let with_icon = descriptor("alpha", "com.example.alpha").replace(
            "icon = \"agent\"",
            "icon = \"alpha\"\nicon_asset = \"assets/icon.svg\"\nicon_source = \"internal:test-fixture\"\nicon_license = \"CC0-1.0\"",
        );
        fs::write(&source, &with_icon).unwrap();
        fs::write(
            dir.join("assets/icon.svg"),
            "<svg viewBox=\"0 0 1 1\"><path d=\"M0 0h1v1z\"/></svg>\n",
        )
        .unwrap();
        let runtime = parse_runtime_descriptor("alpha", &source, &with_icon).unwrap();
        assert!(validate_runtime_descriptors(vec![runtime]).is_ok());

        let without_provenance = descriptor("alpha", "com.example.alpha").replace(
            "icon = \"agent\"",
            "icon = \"alpha\"\nicon_asset = \"assets/icon.svg\"",
        );
        let runtime = parse_runtime_descriptor("alpha", &source, &without_provenance).unwrap();
        let error = validate_runtime_descriptors(vec![runtime])
            .unwrap_err()
            .to_string();
        assert!(error.contains("requires display.icon_source and display.icon_license"));

        fs::remove_file(dir.join("assets/icon.svg")).unwrap();
        let runtime = parse_runtime_descriptor("alpha", &source, &with_icon).unwrap();
        let error = validate_runtime_descriptors(vec![runtime])
            .unwrap_err()
            .to_string();
        assert!(error.contains("display.icon_asset"));
        assert!(error.contains("cannot be read"));

        let unsafe_path = with_icon.replace("assets/icon.svg", "assets/../icon.svg");
        let runtime = parse_runtime_descriptor("alpha", &source, &unsafe_path).unwrap();
        let error = validate_runtime_descriptors(vec![runtime])
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be a safe SVG path below assets/"));

        let untethered_rendering = descriptor("alpha", "com.example.alpha").replace(
            "icon = \"agent\"",
            "icon = \"agent\"\nicon_template = false",
        );
        let runtime = parse_runtime_descriptor("alpha", &source, &untethered_rendering).unwrap();
        let error = validate_runtime_descriptors(vec![runtime])
            .unwrap_err()
            .to_string();
        assert!(error.contains("icon_template = false requires display.icon_asset"));
    }

    #[test]
    fn rejects_bad_schema() {
        let text = descriptor("alpha", "com.example.alpha").replacen(
            "schema_version = 1",
            "schema_version = 2",
            1,
        );
        let parsed = parse_runtime_descriptor("alpha", "/alpha/runtime.toml", &text).unwrap();
        let error = validate_runtime_descriptors(vec![parsed]).unwrap_err();
        assert!(error.to_string().contains("unsupported schema_version 2"));
    }

    #[test]
    fn rejects_duplicate_ids_and_slugs() {
        let one = parsed("alpha", "com.example.same");
        let mut duplicate_id = parsed("beta", "com.example.same");
        duplicate_id.descriptor.detection.command_aliases = vec!["beta".into()];
        duplicate_id.descriptor.detection.process_aliases = vec!["beta".into()];
        let mut duplicate_slug = parsed("gamma", "com.example.gamma");
        duplicate_slug.directory_slug = "alpha".into();
        duplicate_slug.descriptor.slug = "alpha".into();
        duplicate_slug.descriptor.legacy_slug = "alpha".into();
        duplicate_slug.descriptor.detection.command_aliases = vec!["gamma".into(), "alpha".into()];
        duplicate_slug.descriptor.detection.process_aliases = vec!["gamma".into()];
        let error = validate_runtime_descriptors(vec![one, duplicate_id, duplicate_slug])
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicate runtime id 'com.example.same'"));
        assert!(error.contains("duplicate package slug 'alpha'"));
        assert!(error.contains("duplicate legacy_slug 'alpha'"));
    }

    #[test]
    fn validates_alias_normalization_duplicates_and_cross_runtime_ownership() {
        let mut one = parsed("alpha", "com.example.alpha");
        one.descriptor.detection.command_aliases =
            vec!["alpha".into(), "Alpha".into(), "alpha".into()];
        let mut two = parsed("beta", "com.example.beta");
        two.descriptor
            .detection
            .process_aliases
            .push("alpha".into());
        let error = validate_runtime_descriptors(vec![one, two])
            .unwrap_err()
            .to_string();
        assert!(error.contains("command alias 'Alpha' must be a normalized executable basename"));
        assert!(error.contains("duplicate command alias 'alpha'"));
        assert!(error.contains("alias 'alpha' conflicts with runtime 'com.example.alpha'"));
    }

    #[test]
    fn unknown_capability_fails_toml_decoding() {
        let text = descriptor("alpha", "com.example.alpha")
            .replace("\"notify_when_done\"", "\"teleport\"");
        let error = parse_runtime_descriptor("alpha", "/alpha/runtime.toml", &text)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown variant `teleport`"));
    }

    #[test]
    fn rejects_duplicate_capabilities() {
        let mut runtime = parsed("alpha", "com.example.alpha");
        runtime
            .descriptor
            .capabilities
            .push(RuntimeCapability::Resume);
        let error = validate_runtime_descriptors(vec![runtime])
            .unwrap_err()
            .to_string();
        assert!(error.contains("capabilities contains duplicate values"));
    }

    #[test]
    fn adapter_free_metadata_rejects_every_adapter_owned_capability() {
        let mut metadata = parsed("metadata", "com.example.metadata");
        metadata.descriptor.adapter = None;
        metadata.descriptor.legacy_order = None;
        metadata.descriptor.capabilities.clear();
        metadata.descriptor.lifecycle.source = RuntimeLifecycleSource::Output;
        metadata.descriptor.lifecycle.authority = RuntimeLifecycleAuthority::None;
        metadata.descriptor.lifecycle.fallback = RuntimeLifecycleFallback::Output;
        metadata.descriptor.lifecycle.completion_reliable = false;
        metadata.descriptor.lifecycle.attention_reliable = false;
        metadata.descriptor.suggested_presets = vec![RuntimeSuggestedPreset {
            id: "metadata-default".into(),
            label: "Metadata".into(),
            command: "metadata".into(),
            quick_launch: true,
        }];
        let error = validate_runtime_descriptors(vec![metadata.clone()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("authority = 'none' must use fallback = 'none'"));
        metadata.descriptor.lifecycle.fallback = RuntimeLifecycleFallback::None;
        assert!(
            validate_runtime_descriptors(vec![metadata.clone()]).is_ok(),
            "detection, presentation, and presets must remain valid without a compiled adapter"
        );

        for capability in [
            RuntimeCapability::LifecycleHooks,
            RuntimeCapability::Resume,
            RuntimeCapability::RestartAgent,
            RuntimeCapability::McpSessions,
            RuntimeCapability::McpBrowser,
            RuntimeCapability::McpComputer,
            RuntimeCapability::NotifyWhenDone,
        ] {
            let mut invalid = metadata.clone();
            invalid.descriptor.capabilities = vec![capability];
            if capability == RuntimeCapability::LifecycleHooks {
                invalid.descriptor.lifecycle.source = RuntimeLifecycleSource::Hooks;
                invalid.descriptor.lifecycle.authority = RuntimeLifecycleAuthority::Complete;
            }
            if capability == RuntimeCapability::NotifyWhenDone {
                invalid.descriptor.lifecycle.completion_reliable = true;
            }
            let error = validate_runtime_descriptors(vec![invalid])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("adapter-owned capabilities")
                    && error.contains(capability.descriptor_name()),
                "missing adapter rejection for {}: {error}",
                capability.descriptor_name()
            );
        }
    }

    #[test]
    fn discovery_and_generated_source_are_deterministic() {
        let temp = TempDir::new().unwrap();
        for (order, (slug, id)) in [("zeta", "com.example.zeta"), ("alpha", "com.example.alpha")]
            .into_iter()
            .enumerate()
        {
            let dir = temp.path().join(slug);
            fs::create_dir_all(&dir).unwrap();
            let contents = descriptor(slug, id).replacen(
                "legacy_order = 1",
                &format!("legacy_order = {order}"),
                1,
            );
            fs::write(dir.join("runtime.toml"), contents).unwrap();
        }
        let first = discover_runtime_descriptors(temp.path()).unwrap();
        let second = discover_runtime_descriptors(temp.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .iter()
                .map(|runtime| runtime.descriptor.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert_eq!(
            generated_catalog_source(&first),
            generated_catalog_source(&second)
        );
        assert_eq!(
            generated_integration_source(&first),
            generated_integration_source(&second)
        );
        assert_eq!(
            generated_transcript_source(&first),
            generated_transcript_source(&second)
        );
        let generated = generated_catalog_source(&first);
        assert!(
            generated.find("/alpha/runtime.toml").unwrap()
                < generated.find("/zeta/runtime.toml").unwrap()
        );
    }

    #[test]
    fn generated_presets_include_adapter_free_runtime_metadata() {
        let mut adapted = parsed("adapted", "com.example.adapted");
        adapted.descriptor.legacy_order = Some(0);
        adapted.descriptor.suggested_presets = vec![RuntimeSuggestedPreset {
            id: "adapted-default".into(),
            label: "Adapted".into(),
            command: "adapted".into(),
            quick_launch: true,
        }];

        let mut metadata = parsed("metadata", "com.example.metadata");
        metadata.descriptor.adapter = None;
        metadata.descriptor.legacy_order = None;
        metadata.descriptor.suggested_presets = vec![RuntimeSuggestedPreset {
            id: "metadata-default".into(),
            label: "Metadata".into(),
            command: "metadata --safe".into(),
            quick_launch: false,
        }];

        let generated = generated_integration_source(&[metadata.clone(), adapted.clone()]);
        assert!(generated.contains("runtimes/adapted/adapter/mod.rs"));
        assert!(!generated.contains("runtimes/metadata/adapter/mod.rs"));
        assert!(generated.contains("\"adapted-default\""));
        assert!(generated.contains("\"metadata-default\""));
        assert!(
            generated.find("\"adapted-default\"").unwrap()
                < generated.find("\"metadata-default\"").unwrap()
        );
        assert_eq!(
            generated,
            generated_integration_source(&[adapted, metadata]),
            "preset and adapter generation must not depend on discovery order"
        );
    }

    #[test]
    fn generated_behavior_and_presets_are_scoped_to_declared_platforms() {
        let mut macos = parsed("macos-only", "com.example.macos-only");
        macos.descriptor.legacy_order = Some(0);
        macos.descriptor.platforms = vec![RuntimePlatform::Macos];
        macos
            .descriptor
            .capabilities
            .push(RuntimeCapability::Transcript);
        macos.descriptor.suggested_presets = vec![RuntimeSuggestedPreset {
            id: "macos-only-default".into(),
            label: "macOS only".into(),
            command: "macos-only".into(),
            quick_launch: true,
        }];

        let mut linux = parsed("linux-only", "com.example.linux-only");
        linux.descriptor.legacy_order = Some(1);
        linux.descriptor.platforms = vec![RuntimePlatform::Linux];
        linux
            .descriptor
            .capabilities
            .push(RuntimeCapability::Transcript);
        linux.descriptor.suggested_presets = vec![RuntimeSuggestedPreset {
            id: "linux-only-default".into(),
            label: "Linux only".into(),
            command: "linux-only".into(),
            quick_launch: true,
        }];

        let descriptors = [macos, linux];
        let integrations = generated_integration_source(&descriptors);
        for (target, module, preset) in [
            ("macos", "macos_only", "macos-only-default"),
            ("linux", "linux_only", "linux-only-default"),
        ] {
            let cfg = format!("#[cfg(target_os = {target:?})]");
            assert!(integrations.contains(&format!("{cfg}\npub(crate) mod {module}")));
            assert!(integrations.contains(&format!("{cfg}\n    \"{preset}\"")));
            assert!(integrations.contains(&format!(
                "{cfg}\n    (\"{}\", {module}::INTEGRATION)",
                module.replace('_', "-")
            )));
        }

        let transcripts = generated_transcript_source(&descriptors);
        assert!(transcripts.contains("#[cfg(target_os = \"macos\")]\nmod transcript_macos_only"));
        assert!(transcripts.contains("#[cfg(target_os = \"linux\")]\nmod transcript_linux_only"));
        assert!(transcripts
            .contains("#[cfg(target_os = \"macos\")]\n    &transcript_macos_only::ADAPTER"));
        assert!(transcripts
            .contains("#[cfg(target_os = \"linux\")]\n    &transcript_linux_only::ADAPTER"));
    }

    #[test]
    fn transcript_capability_and_adapter_file_must_agree() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("alpha");
        fs::create_dir_all(dir.join("adapter")).unwrap();
        let source = dir.join("runtime.toml");
        let text = descriptor("alpha", "com.example.alpha");
        fs::write(&source, &text).unwrap();
        fs::write(dir.join("adapter/transcript.rs"), "// adapter\n").unwrap();
        let parsed = parse_runtime_descriptor("alpha", &source, &text).unwrap();
        let error = validate_runtime_descriptors(vec![parsed])
            .unwrap_err()
            .to_string();
        assert!(error.contains("requires the transcript capability"));

        let with_capability = text.replace(
            "\"restart_agent\", \"notify_when_done\"",
            "\"restart_agent\", \"transcript\", \"notify_when_done\"",
        );
        let parsed = parse_runtime_descriptor("alpha", &source, &with_capability).unwrap();
        let validated = validate_runtime_descriptors(vec![parsed]).unwrap();
        let generated = generated_transcript_source(&validated);
        assert!(generated.contains("runtimes/alpha/adapter/transcript.rs"));
        assert!(generated.contains("transcript_alpha::ADAPTER"));

        let mut transcript_only =
            parse_runtime_descriptor("alpha", &source, &with_capability).unwrap();
        transcript_only.descriptor.adapter = None;
        transcript_only.descriptor.legacy_order = None;
        transcript_only.descriptor.capabilities = vec![RuntimeCapability::Transcript];
        transcript_only.descriptor.lifecycle.source = RuntimeLifecycleSource::Output;
        transcript_only.descriptor.lifecycle.authority = RuntimeLifecycleAuthority::None;
        transcript_only.descriptor.lifecycle.fallback = RuntimeLifecycleFallback::None;
        transcript_only.descriptor.lifecycle.completion_reliable = false;
        assert!(
            validate_runtime_descriptors(vec![transcript_only]).is_ok(),
            "a package-local transcript adapter remains valid without a main integration adapter"
        );

        fs::remove_file(dir.join("adapter/transcript.rs")).unwrap();
        let parsed = parse_runtime_descriptor("alpha", &source, &with_capability).unwrap();
        let error = validate_runtime_descriptors(vec![parsed])
            .unwrap_err()
            .to_string();
        assert!(error.contains("transcript capability requires"));
    }

    #[test]
    fn search_path_suffixes_must_be_safe_relative_paths() {
        let mut runtime = parsed("alpha", "com.example.alpha");
        runtime.descriptor.detection.search_path_suffixes = vec![".agent/bin".into()];
        assert!(validate_runtime_descriptors(vec![runtime.clone()]).is_ok());

        for unsafe_suffix in ["", "/tmp/bin", "../bin", "agent//bin", "agent\\bin", "."] {
            let mut invalid = runtime.clone();
            invalid.descriptor.detection.search_path_suffixes = vec![unsafe_suffix.into()];
            let error = validate_runtime_descriptors(vec![invalid])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("must be a safe non-empty relative path"),
                "unexpected validation result for {unsafe_suffix:?}: {error}"
            );
        }
    }

    #[test]
    fn inherited_environment_keys_must_be_normalized() {
        let mut runtime = parsed("alpha", "com.example.alpha");
        runtime.descriptor.environment.strip_inherited = vec!["AGENT_SESSION_ID".into()];
        assert!(validate_runtime_descriptors(vec![runtime.clone()]).is_ok());

        for invalid_key in ["", "lowercase", "HAS-DASH", "1STARTS_WITH_DIGIT"] {
            let mut invalid = runtime.clone();
            invalid.descriptor.environment.strip_inherited = vec![invalid_key.into()];
            let error = validate_runtime_descriptors(vec![invalid])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("must be a normalized environment name"),
                "unexpected validation result for {invalid_key:?}: {error}"
            );
        }
    }

    #[test]
    fn usage_store_hints_must_be_safe_home_relative_paths() {
        let mut runtime = parsed("alpha", "com.example.alpha");
        runtime.descriptor.usage.stores = vec![RuntimeUsageStore {
            root: ".agent/sessions".into(),
            extensions: vec!["jsonl".into()],
            file_name: None,
            file_name_suffix: Some(".messages.jsonl".into()),
            parent_dir_name: None,
        }];
        assert!(validate_runtime_descriptors(vec![runtime.clone()]).is_ok());

        for unsafe_root in ["", "/tmp/sessions", "../sessions", "agent//sessions"] {
            let mut invalid = runtime.clone();
            invalid.descriptor.usage.stores[0].root = unsafe_root.into();
            let error = validate_runtime_descriptors(vec![invalid])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("safe non-empty home-relative path"),
                "unexpected validation result for {unsafe_root:?}: {error}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_legacy_order_and_script_signatures() {
        let one = parsed("alpha", "com.example.alpha");
        let mut two = parsed("beta", "com.example.beta");
        two.descriptor.legacy_order = one.descriptor.legacy_order;
        let signature = vec![vec!["scope".into(), "agent".into()]];
        let mut one = one;
        one.descriptor.detection.script_path_signatures = signature
            .iter()
            .cloned()
            .map(|components| RuntimeScriptPathSignature { components })
            .collect();
        two.descriptor.detection.script_path_signatures = signature
            .into_iter()
            .map(|components| RuntimeScriptPathSignature { components })
            .collect();
        let error = validate_runtime_descriptors(vec![one, two])
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicate legacy_order"));
        assert!(error.contains("script path component set"));
        assert!(error.contains("conflicts with runtime"));
    }

    #[test]
    fn rejects_unsafe_or_colliding_adapter_module_identifiers() {
        let mut numeric = parsed("numeric", "com.example.numeric");
        numeric.descriptor.legacy_slug = "1agent".into();
        numeric.descriptor.adapter = Some("builtin:1agent".into());
        numeric
            .descriptor
            .detection
            .command_aliases
            .push("1agent".into());
        numeric.descriptor.legacy_order = Some(0);

        let mut hyphen = parsed("hyphen", "com.example.hyphen");
        hyphen.descriptor.legacy_slug = "same-name".into();
        hyphen.descriptor.adapter = Some("builtin:same-name".into());
        hyphen
            .descriptor
            .detection
            .command_aliases
            .push("same-name".into());
        hyphen.descriptor.legacy_order = Some(1);

        let mut underscore = parsed("underscore", "com.example.underscore");
        underscore.descriptor.legacy_slug = "same_name".into();
        underscore.descriptor.adapter = Some("builtin:same_name".into());
        underscore
            .descriptor
            .detection
            .command_aliases
            .push("same_name".into());
        underscore.descriptor.legacy_order = Some(2);

        let error = validate_runtime_descriptors(vec![numeric, hyphen, underscore])
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not produce a valid Rust adapter module identifier"));
        assert!(error.contains("adapter module identifier 'same_name' conflicts"));
    }
}
