//! Guard for the portable Controller core feature set.
//!
//! `unpeel-core` compiles in two shapes: the default `native-host` build every
//! Host binary uses, and `--no-default-features --features controller-core`,
//! the transport-neutral slice a Controller (Swift bridge tests, a wasm32
//! Controller) builds. A module gated `controller-core` must never reach into
//! a `native-host` module from ungated code — the failure only shows up in the
//! portable build (`scripts/ci/check-portable-core.sh`), which a Host-only
//! `cargo test` never runs. This test reproduces that check lexically from
//! `lib.rs`, so it fails in the ordinary workspace test run too.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    /// Always compiled, or gated on `controller-core` only.
    Portable,
    /// Gated on `native-host`.
    NativeHost,
}

fn module_gates(lib_source: &str) -> BTreeMap<String, Gate> {
    let mut gates = BTreeMap::new();
    let mut pending_native = false;
    let mut pending_test_only = false;
    for line in lib_source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[cfg(") {
            if trimmed.contains("feature = \"native-host\"") {
                pending_native = true;
            }
            if trimmed.contains("test") {
                pending_test_only = true;
            }
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        let declaration = trimmed
            .strip_prefix("pub mod ")
            .or_else(|| trimmed.strip_prefix("mod "))
            .and_then(|rest| rest.strip_suffix(';'));
        if let Some(name) = declaration {
            // Test-only modules (this file included) are not part of the
            // library either way; they may name anything in their fixtures.
            if !pending_test_only {
                let gate = if pending_native {
                    Gate::NativeHost
                } else {
                    Gate::Portable
                };
                gates.insert(name.to_owned(), gate);
            }
        }
        // Any non-attribute line consumes the pending attributes, whether or
        // not it was a module declaration (`pub use rustls;` and friends).
        pending_native = false;
        pending_test_only = false;
    }
    gates
}

/// Line numbers (1-based) that sit inside an item introduced by a
/// `#[cfg(feature = "native-host")]` attribute: the attribute line itself,
/// the item's header, and its brace-delimited body. Good enough for rustfmt
/// output; a reference the scanner cannot place counts as ungated.
fn native_gated_lines(source: &str) -> Vec<bool> {
    let lines: Vec<&str> = source.lines().collect();
    let mut gated = vec![false; lines.len()];
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if !(trimmed.starts_with("#[cfg(") && trimmed.contains("feature = \"native-host\"")) {
            index += 1;
            continue;
        }
        let start = index;
        // Skip further attributes and doc comments to the item header.
        let mut header = index + 1;
        while header < lines.len()
            && (lines[header].trim().starts_with('#') || lines[header].trim().starts_with("///"))
        {
            header += 1;
        }
        let mut depth: i64 = 0;
        let mut end = header;
        let mut opened = false;
        while end < lines.len() {
            for byte in lines[end].bytes() {
                match byte {
                    b'{' => {
                        depth += 1;
                        opened = true;
                    }
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            if opened && depth <= 0 {
                break;
            }
            if !opened && lines[end].trim_end().ends_with(';') {
                break;
            }
            end += 1;
        }
        let last = end.min(lines.len() - 1);
        gated[start..=last].iter_mut().for_each(|flag| *flag = true);
        index = end + 1;
    }
    gated
}

fn module_source(src_dir: &Path, module: &str) -> Option<String> {
    let flat = src_dir.join(format!("{module}.rs"));
    let nested = src_dir.join(module).join("mod.rs");
    fs::read_to_string(&flat)
        .or_else(|_| fs::read_to_string(&nested))
        .ok()
}

fn native_references(source: &str, native_modules: &[&str]) -> Vec<(usize, String)> {
    let gated = native_gated_lines(source);
    let mut offenders = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if gated[index] {
            continue;
        }
        let mut rest = line;
        while let Some(position) = rest.find("crate::") {
            let after = &rest[position + "crate::".len()..];
            let ident: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if native_modules.contains(&ident.as_str()) {
                offenders.push((index + 1, ident.clone()));
            }
            rest = &after[ident.len()..];
        }
    }
    offenders
}

#[test]
fn portable_modules_never_reach_into_native_host_modules_ungated() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib_source = fs::read_to_string(src_dir.join("lib.rs")).expect("lib.rs readable");
    let gates = module_gates(&lib_source);
    assert!(
        gates.get("controller_protocol") == Some(&Gate::Portable)
            && gates.get("remote_session_backend") == Some(&Gate::Portable)
            && gates.get("app_resources") == Some(&Gate::Portable)
            && gates.get("app_installer") == Some(&Gate::NativeHost)
            && gates.get("apps_mcp") == Some(&Gate::NativeHost),
        "lib.rs module gating did not parse as expected: {gates:?}"
    );

    let native_modules: Vec<&str> = gates
        .iter()
        .filter(|(_, gate)| **gate == Gate::NativeHost)
        .map(|(name, _)| name.as_str())
        .collect();
    let mut report = Vec::new();
    for (module, gate) in &gates {
        if *gate != Gate::Portable {
            continue;
        }
        let Some(source) = module_source(&src_dir, module) else {
            continue;
        };
        for (line, target) in native_references(&source, &native_modules) {
            report.push(format!(
                "src/{module}.rs:{line}: `crate::{target}` is native-host-only; gate the item with \
                 #[cfg(feature = \"native-host\")] or move the shared piece into a portable module"
            ));
        }
    }
    assert!(
        report.is_empty(),
        "portable Controller core references Host-only modules:\n{}",
        report.join("\n")
    );
}

#[test]
fn gate_scanner_recognizes_native_host_items() {
    let source = "\
pub fn portable() {}

#[cfg(feature = \"native-host\")]
pub fn host_only() {
    crate::app_installer::release_target();
}

#[cfg(feature = \"native-host\")]
pub use crate::state::AppState;

fn leak() {
    let _ = crate::apps_mcp::installed_apps();
}
";
    let offenders = native_references(source, &["app_installer", "apps_mcp", "state"]);
    assert_eq!(offenders, vec![(12, "apps_mcp".to_owned())]);
}

#[test]
fn gate_parser_reads_lib_rs_declarations() {
    let lib = "\
#[cfg(feature = \"native-host\")]
pub mod app_installer;
#[cfg(feature = \"controller-core\")]
pub mod controller_protocol;
pub mod relay_connection;
#[cfg(feature = \"native-host\")]
pub use rustls;
#[cfg(feature = \"controller-core\")]
pub mod relay_wire;
#[cfg(all(test, feature = \"controller-core\"))]
mod portable_gating_tests;
";
    let gates = module_gates(lib);
    assert_eq!(gates.get("portable_gating_tests"), None);
    assert_eq!(gates.get("app_installer"), Some(&Gate::NativeHost));
    assert_eq!(gates.get("controller_protocol"), Some(&Gate::Portable));
    assert_eq!(gates.get("relay_connection"), Some(&Gate::Portable));
    assert_eq!(gates.get("relay_wire"), Some(&Gate::Portable));
    assert_eq!(gates.get("rustls"), None);
}
