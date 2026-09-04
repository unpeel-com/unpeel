use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
struct MetadataRequest {
    manifest_path: PathBuf,
    packages: Vec<String>,
}

#[derive(Debug)]
struct Args {
    requests: Vec<MetadataRequest>,
    targets: Vec<String>,
    output: PathBuf,
}

#[derive(Clone, Debug)]
struct Package {
    id: String,
    name: String,
    version: String,
    license: Option<String>,
    license_file: Option<PathBuf>,
    manifest_path: PathBuf,
    source: Option<String>,
}

impl Package {
    fn display_name(&self) -> String {
        match self.license.as_deref() {
            Some(license) => format!("{} {} ({license})", self.name, self.version),
            None => format!(
                "{} {} (license expression not declared)",
                self.name, self.version
            ),
        }
    }

    fn stable_key(&self) -> String {
        match self.source.as_deref() {
            Some(source) => format!("{}\0{}\0{source}", self.name, self.version),
            None => format!(
                "{}\0{}\0{}",
                self.name,
                self.version,
                self.manifest_path.display()
            ),
        }
    }
}

#[derive(Debug)]
struct LicenseUse {
    package: String,
    file: String,
}

#[derive(Debug)]
struct LicenseFile {
    path: PathBuf,
    label: Option<String>,
    expected_sha256: Option<&'static str>,
}

#[derive(Debug)]
struct Notice {
    body: Vec<u8>,
    uses: Vec<LicenseUse>,
}

fn usage() -> &'static str {
    "usage: unpeel-license-notices \\\n  --manifest-path <Cargo.toml> --package <name> [--package <name> ...] \\\n  [--manifest-path <Cargo.toml> --package <name> ...] \\\n  --target <rust-target> [--target <rust-target> ...] --output <file>\n\n\
Each --package belongs to the preceding --manifest-path. The collector runs \
`cargo metadata --locked` once per manifest/target pair, follows the selected \
packages' complete normal/runtime dependency graphs, and excludes workspace-\
owned packages plus dev/build-only tool edges."
}

fn parse_args() -> Result<Args, String> {
    let mut requests = Vec::<MetadataRequest>::new();
    let mut targets = Vec::new();
    let mut output = None;
    let mut args = env::args_os().skip(1);

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--manifest-path") => {
                let value = args
                    .next()
                    .ok_or_else(|| "--manifest-path requires a value".to_string())?;
                requests.push(MetadataRequest {
                    manifest_path: PathBuf::from(value),
                    packages: Vec::new(),
                });
            }
            Some("--package") => {
                let value = args
                    .next()
                    .ok_or_else(|| "--package requires a value".to_string())?;
                let value = value
                    .into_string()
                    .map_err(|_| "--package must be valid UTF-8".to_string())?;
                requests
                    .last_mut()
                    .ok_or_else(|| "--package must follow --manifest-path".to_string())?
                    .packages
                    .push(value);
            }
            Some("--target") => {
                let value = args
                    .next()
                    .ok_or_else(|| "--target requires a value".to_string())?;
                targets.push(
                    value
                        .into_string()
                        .map_err(|_| "--target must be valid UTF-8".to_string())?,
                );
            }
            Some("--output") => {
                let value = args
                    .next()
                    .ok_or_else(|| "--output requires a value".to_string())?;
                if output.replace(PathBuf::from(value)).is_some() {
                    return Err("--output may only be passed once".to_string());
                }
            }
            Some("--help" | "-h") => {
                println!("{}", usage());
                std::process::exit(0);
            }
            _ => return Err(format!("unexpected argument: {}", arg.to_string_lossy())),
        }
    }

    if requests.is_empty() {
        return Err("at least one --manifest-path is required".to_string());
    }
    for request in &requests {
        if request.packages.is_empty() {
            return Err(format!(
                "{} has no selected --package",
                request.manifest_path.display()
            ));
        }
    }
    if targets.is_empty() {
        return Err("at least one --target is required".to_string());
    }
    targets.sort();
    targets.dedup();

    Ok(Args {
        requests,
        targets,
        output: output.ok_or_else(|| "--output is required".to_string())?,
    })
}

fn required_str<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("cargo metadata omitted {field} for {context}"))
}

fn package_from_json(value: &Value) -> Result<Package, String> {
    let name = required_str(value, "name", "a package")?.to_string();
    let version = required_str(value, "version", &name)?.to_string();
    Ok(Package {
        id: required_str(value, "id", &name)?.to_string(),
        name,
        version,
        license: value
            .get("license")
            .and_then(Value::as_str)
            .map(str::to_string),
        license_file: value
            .get("license_file")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        manifest_path: PathBuf::from(required_str(value, "manifest_path", "a package")?),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn runtime_dependency_id<'a>(
    dependency: &'a Value,
    parent: &str,
) -> Result<Option<&'a str>, String> {
    let package_id = required_str(dependency, "pkg", parent)?;
    let kinds = dependency
        .get("dep_kinds")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("cargo metadata omitted dep_kinds for {parent} -> {package_id}"))?;
    if kinds.is_empty() {
        return Err(format!(
            "cargo metadata reported no dependency kinds for {parent} -> {package_id}"
        ));
    }

    // `null` is Cargo metadata's wire value for a normal dependency. Dev and
    // build dependencies affect tests/build caches but are not distributed in
    // the linked app or CLI, so do not pull those tool-only subgraphs into the
    // customer-facing notice payload.
    Ok(kinds
        .iter()
        .any(|kind| kind.get("kind").is_some_and(Value::is_null))
        .then_some(package_id))
}

fn collect_metadata(
    request: &MetadataRequest,
    target: &str,
    collected: &mut BTreeMap<String, Package>,
) -> Result<(), String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsStr::new("cargo").to_owned());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            target,
            "--manifest-path",
        ])
        .arg(&request.manifest_path)
        .output()
        .map_err(|error| {
            format!(
                "failed to start cargo metadata for {} ({target}): {error}",
                request.manifest_path.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata --locked failed for {} ({target}):\n{}",
            request.manifest_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "invalid cargo metadata JSON for {} ({target}): {error}",
            request.manifest_path.display()
        )
    })?;

    let packages_json = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata omitted packages".to_string())?;
    let packages = packages_json
        .iter()
        .map(package_from_json)
        .collect::<Result<Vec<_>, _>>()?;
    let by_id = packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let workspace_members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata omitted workspace_members".to_string())?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();

    let nodes = metadata
        .get("resolve")
        .and_then(|resolve| resolve.get("nodes"))
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata omitted resolve.nodes".to_string())?;
    let mut dependencies = BTreeMap::<&str, Vec<&str>>::new();
    for node in nodes {
        let id = required_str(node, "id", "a resolve node")?;
        let deps = node
            .get("deps")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("cargo metadata omitted deps for {id}"))?
            .iter()
            .map(|dependency| runtime_dependency_id(dependency, id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        dependencies.insert(id, deps);
    }

    let mut roots = Vec::new();
    for requested_name in &request.packages {
        let matches = packages
            .iter()
            .filter(|package| {
                package.name == *requested_name && workspace_members.contains(package.id.as_str())
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [package] => roots.push(package.id.as_str()),
            [] => {
                return Err(format!(
                    "selected package {requested_name} is not a workspace member of {}",
                    request.manifest_path.display()
                ))
            }
            _ => {
                return Err(format!(
                    "selected package {requested_name} is ambiguous in {}",
                    request.manifest_path.display()
                ))
            }
        }
    }

    let mut pending = roots;
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let package = by_id
            .get(id)
            .ok_or_else(|| format!("resolve node references unknown package {id}"))?;
        if !workspace_members.contains(id) {
            collected
                .entry(package.stable_key())
                .or_insert_with(|| (*package).clone());
        }
        if let Some(deps) = dependencies.get(id) {
            pending.extend(deps.iter().copied());
        }
    }

    Ok(())
}

fn is_license_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["license", "licence", "copying", "notice", "unlicense"]
        .iter()
        .any(|prefix| {
            lower == *prefix
                || lower.starts_with(&format!("{prefix}-"))
                || lower.starts_with(&format!("{prefix}."))
        })
}

fn discover_license_files(
    directory: &Path,
    depth: usize,
    inside_licenses_directory: bool,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    if depth > 4 {
        return Ok(());
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if matches!(name.as_ref(), ".git" | ".hg" | ".svn" | "target") {
                continue;
            }
            let is_licenses_directory = inside_licenses_directory
                || matches!(name.to_ascii_lowercase().as_str(), "license" | "licenses");
            discover_license_files(&path, depth + 1, is_licenses_directory, files)?;
        } else if (file_type.is_file() || file_type.is_symlink())
            && (is_license_name(&name) || inside_licenses_directory)
        {
            files.insert(path);
        }
    }
    Ok(())
}

fn approved_license_overrides(package: &Package) -> Option<Vec<LicenseFile>> {
    // serial 0.4.0's published top-level crate accidentally omitted the
    // repository LICENSE. This is deliberately an exact, checksum-pinned
    // exception rather than a generic license-expression fallback. Details
    // and provenance live next to the audited source text.
    if package.name == "serial"
        && package.version == "0.4.0"
        && package.license.as_deref() == Some("MIT")
        && package.source.as_deref()
            == Some("registry+https://github.com/rust-lang/crates.io-index")
    {
        return Some(vec![LicenseFile {
            path: Path::new(env!("CARGO_MANIFEST_DIR")).join("overrides/serial-0.4.0/LICENSE"),
            label: Some("audited override: serial-0.4.0/LICENSE".to_string()),
            expected_sha256: Some(
                "871afd9d691846de71e0b83812ba9c7ff00bc7b3ad102dedcaa109f2246d52ad",
            ),
        }]);
    }
    // yasna 0.5.2's Cargo include list omitted both license files even though
    // its exact source commit contains them. As above, this exception is
    // locked to the package, version, registry, source paths, and checksums.
    if package.name == "yasna"
        && package.version == "0.5.2"
        && package.license.as_deref() == Some("MIT OR Apache-2.0")
        && package.source.as_deref()
            == Some("registry+https://github.com/rust-lang/crates.io-index")
    {
        return Some(vec![
            LicenseFile {
                path: Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("overrides/yasna-0.5.2/LICENSE-APACHE"),
                label: Some("audited override: yasna-0.5.2/LICENSE-APACHE".to_string()),
                expected_sha256: Some(
                    "a60eea817514531668d7e00765731449fe14d059d3249e0bc93b36de45f759f2",
                ),
            },
            LicenseFile {
                path: Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("overrides/yasna-0.5.2/LICENSE-MIT"),
                label: Some("audited override: yasna-0.5.2/LICENSE-MIT".to_string()),
                expected_sha256: Some(
                    "2a7df90e60fd5512b8bca35d3ce90e068f21c52d962855053fd1d9e2e7ca0f16",
                ),
            },
        ]);
    }
    None
}

fn package_license_files(package: &Package) -> Result<Vec<LicenseFile>, String> {
    if package
        .license
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return Err(format!(
            "{} {} has no declared license expression",
            package.name, package.version
        ));
    }
    let root = package
        .manifest_path
        .parent()
        .ok_or_else(|| format!("invalid manifest path for {}", package.display_name()))?;
    let mut files = BTreeSet::new();
    if let Some(license_file) = &package.license_file {
        let license_file = if license_file.is_absolute() {
            license_file.clone()
        } else {
            root.join(license_file)
        };
        files.insert(license_file);
    }
    discover_license_files(root, 0, false, &mut files)?;

    if files.is_empty() {
        if let Some(license_overrides) = approved_license_overrides(package) {
            return Ok(license_overrides);
        }
        return Err(format!(
            "{} has no usable license file in {}",
            package.display_name(),
            root.display()
        ));
    }
    Ok(files
        .into_iter()
        .map(|path| LicenseFile {
            path,
            label: None,
            expected_sha256: None,
        })
        .collect())
}

fn relative_license_name(package: &Package, path: &Path) -> String {
    let root = package
        .manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    path.strip_prefix(root)
        .unwrap_or_else(|_| path.file_name().map(Path::new).unwrap_or(path))
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_notices(packages: BTreeMap<String, Package>) -> Result<Vec<Notice>, String> {
    let mut notices = Vec::<Notice>::new();
    for package in packages.into_values() {
        for license_file in package_license_files(&package)? {
            let path = &license_file.path;
            let body = fs::read(path).map_err(|error| {
                format!(
                    "{} has an unreadable license file {}: {error}",
                    package.display_name(),
                    path.display()
                )
            })?;
            if body.is_empty() {
                return Err(format!(
                    "{} has an empty license file {}",
                    package.display_name(),
                    path.display()
                ));
            }
            if std::str::from_utf8(&body).is_err() {
                return Err(format!(
                    "{} has a non-UTF-8 license file {}",
                    package.display_name(),
                    path.display()
                ));
            }
            if let Some(expected) = license_file.expected_sha256 {
                let actual = format!("{:x}", Sha256::digest(&body));
                if actual != expected {
                    return Err(format!(
                        "{} audited license override {} has SHA-256 {actual}, expected {expected}",
                        package.display_name(),
                        path.display()
                    ));
                }
            }
            let license_use = LicenseUse {
                package: package.display_name(),
                file: license_file
                    .label
                    .unwrap_or_else(|| relative_license_name(&package, path)),
            };
            match notices.iter_mut().find(|notice| notice.body == body) {
                Some(notice) => notice.uses.push(license_use),
                None => notices.push(Notice {
                    body,
                    uses: vec![license_use],
                }),
            }
        }
    }
    Ok(notices)
}

fn render(notices: &[Notice]) -> Vec<u8> {
    let mut output = String::from(
        "UNPEEL THIRD-PARTY NOTICES — RUST\n\
         \n\
         This file is generated deterministically from cargo metadata --locked.\n\
         Identical license texts are included once and list every resolved package that uses them.\n",
    );
    for (index, notice) in notices.iter().enumerate() {
        output.push_str(
            "\n================================================================================\n",
        );
        output.push_str(&format!("NOTICE {}\n", index + 1));
        output.push_str("Packages and source files:\n");
        let mut uses = notice
            .uses
            .iter()
            .map(|license_use| format!("{} — {}", license_use.package, license_use.file))
            .collect::<Vec<_>>();
        uses.sort();
        uses.dedup();
        for license_use in uses {
            output.push_str("- ");
            output.push_str(&license_use);
            output.push('\n');
        }
        output.push_str(
            "--------------------------------------------------------------------------------\n",
        );
        output.push_str(std::str::from_utf8(&notice.body).expect("validated UTF-8"));
        if !output.ends_with('\n') {
            output.push('\n');
        }
    }
    output.into_bytes()
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let mut packages = BTreeMap::new();
    for request in &args.requests {
        for target in &args.targets {
            collect_metadata(request, target, &mut packages)?;
        }
    }
    if packages.is_empty() {
        return Err("selected packages resolved no third-party dependencies".to_string());
    }
    let notices = collect_notices(packages)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(&args.output, render(&notices))
        .map_err(|error| format!("cannot write {}: {error}", args.output.display()))?;
    println!(
        "Wrote {} deduplicated Rust license texts to {}",
        notices.len(),
        args.output.display()
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}\n\n{}", usage());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_license_name, package_license_files, runtime_dependency_id, Package};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn recognizes_common_license_names() {
        for name in [
            "LICENSE",
            "LICENSE-APACHE",
            "License.md",
            "LICENCE.txt",
            "COPYING",
            "NOTICE.third-party",
            "UNLICENSE",
        ] {
            assert!(is_license_name(name), "{name}");
        }
        assert!(!is_license_name("README.md"));
        assert!(!is_license_name("license_header.rs"));
    }

    #[test]
    fn follows_only_normal_runtime_dependency_edges() {
        let normal = json!({"pkg": "normal", "dep_kinds": [{"kind": null, "target": null}]});
        let mixed = json!({
            "pkg": "mixed",
            "dep_kinds": [
                {"kind": "build", "target": null},
                {"kind": null, "target": "cfg(unix)"}
            ]
        });
        let build = json!({"pkg": "build", "dep_kinds": [{"kind": "build", "target": null}]});
        let dev = json!({"pkg": "dev", "dep_kinds": [{"kind": "dev", "target": null}]});

        assert_eq!(
            runtime_dependency_id(&normal, "root").unwrap(),
            Some("normal")
        );
        assert_eq!(
            runtime_dependency_id(&mixed, "root").unwrap(),
            Some("mixed")
        );
        assert_eq!(runtime_dependency_id(&build, "root").unwrap(), None);
        assert_eq!(runtime_dependency_id(&dev, "root").unwrap(), None);
    }

    #[test]
    fn undeclared_licenses_fail_before_file_discovery() {
        let package = Package {
            id: "missing-license".to_string(),
            name: "missing-license".to_string(),
            version: "1.2.3".to_string(),
            license: None,
            license_file: None,
            manifest_path: PathBuf::from("/does/not/need/to/exist/Cargo.toml"),
            source: Some("registry+https://github.com/rust-lang/crates.io-index".to_string()),
        };
        assert_eq!(
            package_license_files(&package).unwrap_err(),
            "missing-license 1.2.3 has no declared license expression"
        );
    }
}
