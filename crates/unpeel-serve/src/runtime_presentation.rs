//! Client-safe runtime presentation resolved from the Host's built-in catalog.
//!
//! This module intentionally exposes no provider behavior. Hooks, resume,
//! context, transcripts, and effective actions remain owned by the Host.

use std::path::Path;

use unpeel_core::runtime_catalog::{builtin_runtime_catalog, RuntimeDescriptor, RuntimeLifecycle};

fn runtime_for_identity(identity: &str) -> Option<&'static RuntimeDescriptor> {
    let catalog = builtin_runtime_catalog();
    catalog
        .by_id(identity)
        .or_else(|| catalog.by_slug(identity))
        .or_else(|| catalog.by_legacy_slug(identity))
        .or_else(|| catalog.by_executable_alias(identity))
}

fn runtime_for_command(command: &str) -> Option<&'static RuntimeDescriptor> {
    let head = unpeel_core::integrations::command_head(command);
    let alias = Path::new(head)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(head);
    builtin_runtime_catalog().by_command_alias(alias)
}

fn local_runtime_for_command(command: &str) -> Option<&'static RuntimeDescriptor> {
    runtime_for_command(command).filter(|runtime| runtime.supports_current_platform())
}

pub fn presentation_command(runtime_id: &str) -> Option<&'static str> {
    runtime_for_identity(runtime_id)?
        .detection
        .command_aliases
        .first()
        .map(String::as_str)
}

pub fn legacy_slug(command: &str) -> Option<&'static str> {
    local_runtime_for_command(command).map(|runtime| runtime.legacy_slug.as_str())
}

/// Controller-side lifecycle policy declared by the runtime package. Unknown
/// terminal commands deliberately have no policy and use generic defaults.
pub fn lifecycle(command: &str) -> Option<&'static RuntimeLifecycle> {
    local_runtime_for_command(command).map(|runtime| &runtime.lifecycle)
}

/// Installed-App display fallback: a locally installed App's `[display]`
/// tint colors its command everywhere the catalog would color a built-in.
/// Built-ins win — `app_runtime` never indexes a reserved alias — and this
/// is presentation data only.
fn app_tint_for_command(command: &str, spinner: bool) -> Option<u32> {
    let app = unpeel_core::app_runtime::app_for_launch_command(command)?;
    let hex = if spinner {
        app.spinner_tint.or(app.tint)
    } else {
        app.tint
    }?;
    parse_hex_color(&hex)
}

pub fn tint_hex(command: &str) -> Option<u32> {
    runtime_for_command(command)
        .and_then(|runtime| runtime.display.tint.as_deref())
        .and_then(parse_hex_color)
        .or_else(|| app_tint_for_command(command, false))
}

pub fn spinner_tint_hex(command: &str) -> Option<u32> {
    runtime_for_command(command)
        .and_then(|runtime| {
            runtime
                .display
                .spinner_tint
                .as_deref()
                .or(runtime.display.tint.as_deref())
                .and_then(parse_hex_color)
        })
        .or_else(|| app_tint_for_command(command, true))
}

pub fn tint_rgb(command: &str) -> Option<(u8, u8, u8)> {
    let hex = tint_hex(command)?;
    Some(((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
}

fn parse_hex_color(value: &str) -> Option<u32> {
    let digits = value.strip_prefix('#')?;
    (digits.len() == 6)
        .then(|| u32::from_str_radix(digits, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_legacy_runtime_ids_resolve_to_presentation_commands() {
        assert_eq!(
            presentation_command("com.anthropic.claude-code"),
            Some("claude")
        );
        assert_eq!(presentation_command("claude"), Some("claude"));
        assert_eq!(presentation_command("unknown.runtime"), None);
    }

    #[test]
    fn command_metadata_uses_catalog_colors_and_legacy_wire_slug() {
        assert_eq!(
            legacy_slug("/usr/local/bin/codex --full-auto"),
            Some("codex")
        );
        assert_eq!(tint_hex("claude"), Some(0xD97757));
        assert_eq!(spinner_tint_hex("cline"), Some(0x98C4FA));
        assert_eq!(tint_hex("npm run dev"), None);
    }

    #[test]
    fn lifecycle_quirks_come_from_runtime_descriptors() {
        let grok = lifecycle("/usr/local/bin/grok --always-approve").unwrap();
        assert!(!grok.anchor_start_event_to_output);
        assert!(!grok.attention_clears_on_output);
        assert!(!grok.distrust_stops_while_output_grows);

        let codex = lifecycle("codex --full-auto").unwrap();
        assert!(codex.anchor_start_event_to_output);
        assert!(codex.attention_clears_on_output);
        assert!(codex.distrust_stops_while_output_grows);

        let claude = lifecycle("claude").unwrap();
        assert!(claude.anchor_start_event_to_output);
        assert!(claude.attention_clears_on_output);
        assert!(!claude.distrust_stops_while_output_grows);
        assert!(lifecycle("npm run dev").is_none());
    }
}
