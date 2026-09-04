//! Host-owned catalog for built-in agent runtimes.
//!
//! Each descriptor is discovered from `runtimes/<package-slug>/runtime.toml`
//! by `build.rs`, validated before Rust compilation, and embedded into the
//! Host. The same build generates the package-local integration and transcript
//! registries; legacy slugs and wire fields remain compatibility projections.

#[path = "runtime_catalog_schema.rs"]
mod schema;

pub use schema::{
    RuntimeCapability, RuntimeCatalogError, RuntimeDescriptor, RuntimeDetection, RuntimeDisplay,
    RuntimeInstall, RuntimeLifecycle, RuntimeLifecycleAuthority, RuntimeLifecycleFallback,
    RuntimeLifecycleSource, RuntimePlatform, RuntimeScriptPathSignature, RuntimeSuggestedPreset,
    RUNTIME_DESCRIPTOR_SCHEMA_VERSION,
};

use std::path::Path;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/runtime_catalog_generated.rs"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCatalog {
    descriptors: Vec<RuntimeDescriptor>,
}

impl RuntimeCatalog {
    pub fn load_from_directory(root: &Path) -> Result<Self, RuntimeCatalogError> {
        let discovered = schema::discover_runtime_descriptors(root)?;
        Ok(Self {
            descriptors: discovered
                .into_iter()
                .map(|runtime| runtime.descriptor)
                .collect(),
        })
    }

    fn from_embedded() -> Result<Self, RuntimeCatalogError> {
        let mut discovered = Vec::with_capacity(BUILTIN_RUNTIME_DESCRIPTOR_TOML.len());
        for (slug, contents) in BUILTIN_RUNTIME_DESCRIPTOR_TOML {
            discovered.push(schema::parse_runtime_descriptor(
                slug,
                format!("runtimes/{slug}/runtime.toml"),
                contents,
            )?);
        }
        let discovered = schema::validate_runtime_descriptors(discovered)?;
        Ok(Self {
            descriptors: discovered
                .into_iter()
                .map(|runtime| runtime.descriptor)
                .collect(),
        })
    }

    pub fn descriptors(&self) -> &[RuntimeDescriptor] {
        &self.descriptors
    }

    /// Descriptors remain globally available so a Controller can present a
    /// Session owned by a Host on another platform. Local Host behavior must
    /// use this filtered view (or one of the platform-aware lookups below).
    pub fn descriptors_for_platform(
        &self,
        platform: RuntimePlatform,
    ) -> impl Iterator<Item = &RuntimeDescriptor> {
        self.descriptors
            .iter()
            .filter(move |runtime| runtime.supports_platform(platform))
    }

    pub fn current_platform_descriptors(&self) -> impl Iterator<Item = &RuntimeDescriptor> {
        self.descriptors
            .iter()
            .filter(|runtime| runtime.supports_current_platform())
    }

    pub fn by_id(&self, id: &str) -> Option<&RuntimeDescriptor> {
        self.descriptors.iter().find(|runtime| runtime.id == id)
    }

    pub fn by_legacy_slug(&self, slug: &str) -> Option<&RuntimeDescriptor> {
        self.descriptors
            .iter()
            .find(|runtime| runtime.legacy_slug.eq_ignore_ascii_case(slug.trim()))
    }

    pub fn by_legacy_slug_for_current_platform(&self, slug: &str) -> Option<&RuntimeDescriptor> {
        self.by_legacy_slug(slug)
            .filter(|runtime| runtime.supports_current_platform())
    }

    pub fn by_slug(&self, slug: &str) -> Option<&RuntimeDescriptor> {
        self.descriptors
            .iter()
            .find(|runtime| runtime.slug.eq_ignore_ascii_case(slug.trim()))
    }

    pub fn by_executable_alias(&self, alias: &str) -> Option<&RuntimeDescriptor> {
        let alias = alias.trim();
        self.descriptors.iter().find(|runtime| {
            runtime
                .detection
                .command_aliases
                .iter()
                .chain(&runtime.detection.process_aliases)
                .any(|candidate| candidate.eq_ignore_ascii_case(alias))
        })
    }

    pub fn by_command_alias(&self, alias: &str) -> Option<&RuntimeDescriptor> {
        let alias = alias.trim();
        self.descriptors.iter().find(|runtime| {
            runtime
                .detection
                .command_aliases
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(alias))
        })
    }

    pub fn by_command_alias_for_platform(
        &self,
        alias: &str,
        platform: RuntimePlatform,
    ) -> Option<&RuntimeDescriptor> {
        self.by_command_alias(alias)
            .filter(|runtime| runtime.supports_platform(platform))
    }

    pub fn by_command_alias_for_current_platform(&self, alias: &str) -> Option<&RuntimeDescriptor> {
        self.by_command_alias(alias)
            .filter(|runtime| runtime.supports_current_platform())
    }

    pub fn by_process_alias(&self, alias: &str) -> Option<&RuntimeDescriptor> {
        let alias = alias.trim();
        self.descriptors.iter().find(|runtime| {
            runtime
                .detection
                .process_aliases
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(alias))
        })
    }

    pub fn by_process_alias_for_current_platform(&self, alias: &str) -> Option<&RuntimeDescriptor> {
        self.by_process_alias(alias)
            .filter(|runtime| runtime.supports_current_platform())
    }

    pub fn by_script_path_components(
        &self,
        path_components: &[String],
    ) -> Option<&RuntimeDescriptor> {
        self.descriptors.iter().find(|runtime| {
            runtime
                .detection
                .script_path_signatures
                .iter()
                .any(|signature| {
                    signature.components.iter().all(|required_component| {
                        path_components
                            .iter()
                            .any(|actual| actual.eq_ignore_ascii_case(required_component))
                    })
                })
        })
    }

    pub fn by_script_path_components_for_current_platform(
        &self,
        path_components: &[String],
    ) -> Option<&RuntimeDescriptor> {
        self.by_script_path_components(path_components)
            .filter(|runtime| runtime.supports_current_platform())
    }
}

impl RuntimeDescriptor {
    pub fn supports_platform(&self, platform: RuntimePlatform) -> bool {
        self.platforms.contains(&platform)
    }

    pub fn supports_current_platform(&self) -> bool {
        current_runtime_platform().is_some_and(|platform| self.supports_platform(platform))
    }
}

pub const fn current_runtime_platform() -> Option<RuntimePlatform> {
    #[cfg(target_os = "macos")]
    {
        return Some(RuntimePlatform::Macos);
    }
    #[cfg(target_os = "linux")]
    {
        return Some(RuntimePlatform::Linux);
    }
    #[allow(unreachable_code)]
    None
}

static BUILTIN_RUNTIME_CATALOG: LazyLock<RuntimeCatalog> = LazyLock::new(|| {
    RuntimeCatalog::from_embedded()
        .expect("built-in runtime descriptors were validated by unpeel-core/build.rs")
});

pub fn builtin_runtime_catalog() -> &'static RuntimeCatalog {
    &BUILTIN_RUNTIME_CATALOG
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_sorted_and_alias_addressable() {
        let catalog = builtin_runtime_catalog();
        let slugs = catalog
            .descriptors()
            .iter()
            .map(|runtime| runtime.slug.as_str())
            .collect::<Vec<_>>();
        let mut sorted = slugs.clone();
        sorted.sort_unstable();
        assert_eq!(slugs, sorted);
        assert_eq!(
            catalog
                .by_executable_alias("claude-code")
                .map(|runtime| runtime.legacy_slug.as_str()),
            Some("claude")
        );
        assert_eq!(
            catalog
                .by_id("com.openai.codex")
                .map(|runtime| runtime.legacy_slug.as_str()),
            Some("codex")
        );
        assert_eq!(
            catalog
                .by_process_alias("amp-local")
                .map(|runtime| runtime.legacy_slug.as_str()),
            Some("amp")
        );
        assert_eq!(
            catalog
                .by_script_path_components(&[
                    "node_modules".into(),
                    "@openai".into(),
                    "codex".into(),
                    "bin".into(),
                ])
                .map(|runtime| runtime.legacy_slug.as_str()),
            Some("codex")
        );
    }

    #[test]
    fn platform_lookups_filter_local_behavior_without_hiding_global_metadata() {
        let template = builtin_runtime_catalog()
            .descriptors()
            .first()
            .expect("runtime fixture")
            .clone();
        let mut macos = template.clone();
        macos.id = "com.example.macos-only".into();
        macos.slug = "macos-only".into();
        macos.legacy_slug = "macos-only".into();
        macos.platforms = vec![RuntimePlatform::Macos];
        macos.detection.command_aliases = vec!["macos-only".into()];
        macos.detection.process_aliases = vec!["macos-only".into()];

        let mut linux = template;
        linux.id = "com.example.linux-only".into();
        linux.slug = "linux-only".into();
        linux.legacy_slug = "linux-only".into();
        linux.platforms = vec![RuntimePlatform::Linux];
        linux.detection.command_aliases = vec!["linux-only".into()];
        linux.detection.process_aliases = vec!["linux-only".into()];

        let catalog = RuntimeCatalog {
            descriptors: vec![macos, linux],
        };

        // Cross-Host presentation keeps both descriptors addressable.
        assert!(catalog.by_command_alias("macos-only").is_some());
        assert!(catalog.by_command_alias("linux-only").is_some());

        assert!(catalog
            .by_command_alias_for_platform("macos-only", RuntimePlatform::Macos)
            .is_some());
        assert!(catalog
            .by_command_alias_for_platform("linux-only", RuntimePlatform::Macos)
            .is_none());
        assert!(catalog
            .by_command_alias_for_platform("linux-only", RuntimePlatform::Linux)
            .is_some());
        assert!(catalog
            .by_command_alias_for_platform("macos-only", RuntimePlatform::Linux)
            .is_none());
        assert_eq!(
            catalog
                .descriptors_for_platform(RuntimePlatform::Macos)
                .count(),
            1
        );
        assert_eq!(
            catalog
                .descriptors_for_platform(RuntimePlatform::Linux)
                .count(),
            1
        );
    }
}
