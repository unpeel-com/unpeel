#[cfg(feature = "native-host")]
#[path = "src/runtime_catalog_schema.rs"]
mod runtime_catalog_schema;

#[cfg(feature = "native-host")]
use std::env;
#[cfg(feature = "native-host")]
use std::fs;
#[cfg(feature = "native-host")]
use std::path::PathBuf;

#[cfg(not(feature = "native-host"))]
fn main() {}

#[cfg(feature = "native-host")]
fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let runtimes_root = manifest_dir.join("../../runtimes");
    println!("cargo:rerun-if-changed={}", runtimes_root.display());

    let descriptors = runtime_catalog_schema::discover_runtime_descriptors(&runtimes_root)
        .unwrap_or_else(|error| panic!("invalid built-in runtime catalog:\n{error}"));
    let generated = runtime_catalog_schema::generated_catalog_source(&descriptors);
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("runtime_catalog_generated.rs");
    fs::write(&output, generated).unwrap_or_else(|error| {
        panic!(
            "failed to write generated runtime catalog {}: {error}",
            output.display()
        )
    });
    let generated = runtime_catalog_schema::generated_integration_source(&descriptors);
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("integration_adapters_generated.rs");
    fs::write(&output, generated).unwrap_or_else(|error| {
        panic!(
            "failed to write generated integration registry {}: {error}",
            output.display()
        )
    });
    let generated = runtime_catalog_schema::generated_transcript_source(&descriptors);
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("transcript_adapters_generated.rs");
    fs::write(&output, generated).unwrap_or_else(|error| {
        panic!(
            "failed to write generated transcript registry {}: {error}",
            output.display()
        )
    });

    // Link the vendored libghostty-vt static archive (see vendor/ghostty-vt/
    // README.md) so every host parses PTY output with the same VT engine the
    // desktop and phone render with. Catalog generation deliberately happens
    // before target selection: unsupported targets still need the generated
    // Rust source in order to compile the provider-neutral catalog.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let slice = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", _) => Some("macos-universal"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    };
    let Some(slice) = slice else {
        return;
    };
    println!(
        "cargo:rustc-link-search=native={}/vendor/ghostty-vt/{slice}",
        manifest_dir.display()
    );
    println!("cargo:rustc-link-lib=static=ghostty-vt");
    // simdutf/highway (bundled in the archive) are C++.
    if target_os == "macos" {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!(
        "cargo:rerun-if-changed={}/vendor/ghostty-vt/{slice}/libghostty-vt.a",
        manifest_dir.display()
    );
}
