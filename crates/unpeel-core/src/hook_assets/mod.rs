use crate::app_paths::unpeel_home;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

mod scripts;
pub(crate) use scripts::NOTIFY_HOOK_SCRIPT;

const TRACE_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Append a line to `~/.unpeel/hooks/trace.log`, rotating the file to
/// `trace.log.1` once it grows past `TRACE_LOG_MAX_BYTES` so the trace can
/// never grow without bound. The hook shell scripts append to the same file;
/// rotation here also caps their output because the app appends frequently.
pub fn append_trace_log_line(line: &str) {
    let trace_path = unpeel_home().join("hooks").join("trace.log");
    if let Some(parent) = trace_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(metadata) = fs::metadata(&trace_path) {
        if metadata.len() >= TRACE_LOG_MAX_BYTES {
            let _ = fs::rename(&trace_path, trace_path.with_extension("log.1"));
        }
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&trace_path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Read an existing JSON settings/config file for a merge-and-write update.
///
/// Returns `Ok(Some(object))` when the file is missing/empty (an empty object
/// to merge into) or parses to a JSON object. Returns `Ok(None)` when the file
/// exists with content that is not a valid JSON object — in that case callers
/// MUST skip the update and leave the file untouched, because merging into a
/// fresh `{}` and writing it back would silently destroy the user's real
/// settings (e.g. a `~/.claude/settings.json` with a trailing comma, a comment,
/// or caught torn mid-write by a concurrent writer). Only genuine IO errors
/// surface as `Err`.
pub(crate) fn read_mergeable_json_object(
    path: &Path,
    label: &str,
) -> Result<Option<Value>, String> {
    if !path.exists() {
        return Ok(Some(json!({})));
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("Failed to read {label}: {e}"))?;
    if raw.trim().is_empty() {
        return Ok(Some(json!({})));
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        _ => Ok(None),
    }
}

/// Write `contents` to `path` atomically: write a per-process temp file in the
/// same directory, then rename over the target. Prevents a concurrent reader
/// (or a concurrent Unpeel host spawning another session) from observing a torn
/// half-written settings file.
pub(crate) fn write_file_atomic(path: &Path, contents: &str, label: &str) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unpeel-settings");
    let tmp = path.with_file_name(format!(".{file_name}.unpeel-tmp.{}", std::process::id()));
    fs::write(&tmp, contents).map_err(|e| format!("Failed to write {label}: {e}"))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("Failed to write {label}: {e}"));
    }
    Ok(())
}

/// Open the parent of a project-relative file as a directory descriptor.
/// Every component after the caller-selected project root is resolved with
/// `openat(O_DIRECTORY | O_NOFOLLOW)`. Keeping descriptors for the walk closes
/// the check/use race in a `symlink_metadata` + pathname implementation: even
/// if hosted code swaps a component concurrently, subsequent operations stay
/// anchored in the directory that was actually opened.
#[cfg(unix)]
fn open_project_parent_no_symlinks(
    project_root: &Path,
    relative: &Path,
    create: bool,
) -> Result<Option<(fs::File, std::ffi::CString)>, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => CString::new(value.as_bytes())
                .map_err(|_| "project hook path contains a NUL byte".to_string()),
            _ => Err(format!(
                "project hook path must be relative and may not contain '..': {}",
                relative.display()
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (file_name, parents) = components
        .split_last()
        .ok_or_else(|| "hook install path has no file name".to_string())?;

    let root = CString::new(project_root.as_os_str().as_bytes())
        .map_err(|_| "project root contains a NUL byte".to_string())?;
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "Failed to open project root {}: {}",
            project_root.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut current = unsafe { fs::File::from_raw_fd(root_fd) };

    for component in parents {
        let mut child_fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if child_fd < 0 {
            let open_error = std::io::Error::last_os_error();
            if open_error.kind() == std::io::ErrorKind::NotFound {
                if !create {
                    return Ok(None);
                }
                let mkdir_result =
                    unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o755) };
                if mkdir_result < 0 {
                    let mkdir_error = std::io::Error::last_os_error();
                    if mkdir_error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(format!(
                            "Failed to create a project hook directory: {mkdir_error}"
                        ));
                    }
                }
                child_fd = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        component.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
            }
        }
        if child_fd < 0 {
            return Err(format!(
                "Refusing to install through a non-directory or symlinked project path: {}",
                std::io::Error::last_os_error()
            ));
        }
        current = unsafe { fs::File::from_raw_fd(child_fd) };
    }

    Ok(Some((current, file_name.clone())))
}

/// Write `contents` to `relative` under `project_root`, refusing to follow any
/// symlink in the path chain or at the target itself. Content is written to a
/// new file and renamed relative to the already-open parent descriptor. That
/// avoids both symlink traversal and clobbering through an existing hard link.
#[cfg(unix)]
pub(crate) fn write_project_file_no_symlinks(
    project_root: &Path,
    relative: &Path,
    contents: &[u8],
) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::sync::atomic::{AtomicU64, Ordering};

    let (parent, file_name) = open_project_parent_no_symlinks(project_root, relative, true)?
        .ok_or_else(|| "project hook parent unexpectedly disappeared".to_string())?;

    // Refuse a pre-existing committed leaf symlink. A swap after this check is
    // still harmless: renameat replaces the directory entry; it never follows
    // the entry to an outside target.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    let stat_result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            file_name.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if stat_result == 0 && metadata.st_mode & libc::S_IFMT == libc::S_IFLNK {
        return Err(format!(
            "Refusing to install over symlink {}",
            project_root.join(relative).display()
        ));
    }
    if stat_result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(format!(
                "Failed to inspect project hook target {}: {error}",
                project_root.join(relative).display()
            ));
        }
    }

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut temporary = None;
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = std::ffi::CString::new(format!(
            ".unpeel-hook-tmp.{}.{}",
            std::process::id(),
            sequence
        ))
        .expect("generated project hook temp name has no NUL");
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd >= 0 {
            temporary = Some((name, unsafe { fs::File::from_raw_fd(fd) }));
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(format!(
                "Failed to create project hook temporary file: {error}"
            ));
        }
    }
    let (temporary_name, mut temporary_file) = temporary
        .ok_or_else(|| "Failed to allocate a unique project hook temporary file".to_string())?;

    let cleanup_temporary = || unsafe {
        libc::unlinkat(parent.as_raw_fd(), temporary_name.as_ptr(), 0);
    };
    if let Err(error) = temporary_file
        .write_all(contents)
        .and_then(|_| temporary_file.sync_all())
    {
        cleanup_temporary();
        return Err(format!(
            "Failed to write project hook {}: {error}",
            project_root.join(relative).display()
        ));
    }

    // `renameat` names its source by directory entry rather than by file
    // descriptor. Hosted code with write access to the project could unlink
    // the predictable temporary name after we opened it and replace that name
    // with a symlink or hard link before the rename. Remember the opened
    // inode, keep it alive through the rename, and verify that the installed
    // entry is that exact regular file before reporting success.
    let mut expected: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(temporary_file.as_raw_fd(), &mut expected) } < 0 {
        let error = std::io::Error::last_os_error();
        cleanup_temporary();
        return Err(format!(
            "Failed to inspect project hook temporary file: {error}"
        ));
    }

    let rename_result = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            parent.as_raw_fd(),
            file_name.as_ptr(),
        )
    };
    if rename_result < 0 {
        let error = std::io::Error::last_os_error();
        cleanup_temporary();
        return Err(format!(
            "Failed to install project hook {}: {error}",
            project_root.join(relative).display()
        ));
    }

    let mut installed: libc::stat = unsafe { std::mem::zeroed() };
    let installed_stat = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            file_name.as_ptr(),
            &mut installed,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if installed_stat < 0
        || installed.st_dev != expected.st_dev
        || installed.st_ino != expected.st_ino
        || installed.st_mode & libc::S_IFMT != libc::S_IFREG
    {
        let error = if installed_stat < 0 {
            std::io::Error::last_os_error().to_string()
        } else {
            "temporary directory entry was replaced during installation".to_string()
        };
        if installed_stat == 0 {
            let flags = if installed.st_mode & libc::S_IFMT == libc::S_IFDIR {
                libc::AT_REMOVEDIR
            } else {
                0
            };
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), file_name.as_ptr(), flags);
            }
        }
        return Err(format!(
            "Failed to install project hook {} safely: {error}",
            project_root.join(relative).display()
        ));
    }
    drop(temporary_file);
    let _ = parent.sync_all();
    Ok(())
}

#[cfg(unix)]
fn read_project_file_no_symlinks(
    project_root: &Path,
    relative: &Path,
) -> Result<Option<String>, String> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd, FromRawFd};

    let Some((parent, file_name)) = open_project_parent_no_symlinks(project_root, relative, false)?
    else {
        return Ok(None);
    };
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            file_name.as_ptr(),
            // O_NONBLOCK prevents a hostile FIFO at `.git/info/exclude` from
            // hanging startup before we can reject it below.
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(format!(
            "Refusing to read a symlinked or invalid project file {}: {error}",
            project_root.join(relative).display()
        ));
    }
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut metadata) } < 0
        || metadata.st_mode & libc::S_IFMT != libc::S_IFREG
    {
        return Err(format!(
            "Refusing to read a non-regular project file {}",
            project_root.join(relative).display()
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(|error| {
        format!(
            "Failed to read project file {}: {error}",
            project_root.join(relative).display()
        )
    })?;
    Ok(Some(contents))
}

#[cfg(not(unix))]
pub(crate) fn write_project_file_no_symlinks(
    _project_root: &Path,
    _relative: &Path,
    _contents: &[u8],
) -> Result<(), String> {
    Err("safe project hook installation is unavailable on this platform".to_string())
}

#[cfg(not(unix))]
fn read_project_file_no_symlinks(
    _project_root: &Path,
    _relative: &Path,
) -> Result<Option<String>, String> {
    Err("safe project hook reads are unavailable on this platform".to_string())
}

pub(crate) fn write_executable_script(
    path: &PathBuf,
    contents: &str,
    label: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {} dir {}: {e}", label, parent.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("Failed to write {label}: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|e| format!("Failed to stat {label}: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|e| format!("Failed to chmod {label}: {e}"))?;
    }

    Ok(())
}

pub(crate) fn ensure_project_exclude_entry(cwd: &str, entry: &str) {
    let root = Path::new(cwd);
    let relative = Path::new(".git/info/exclude");
    let existing = match read_project_file_no_symlinks(root, relative) {
        Ok(Some(contents)) => contents,
        Ok(None) => String::new(),
        Err(_) => return,
    };
    if existing.lines().any(|line| line.trim() == entry) {
        return;
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(entry);
    updated.push('\n');
    let _ = write_project_file_no_symlinks(root, relative, updated.as_bytes());
}

/// Shared reporter path used by runtime-owned setup adapters.
pub(crate) fn notify_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("notify-hook.sh")
}

// Public compatibility facade. Runtime-owned setup code moved beside each
// adapter, but these functions were already part of `unpeel_core::hook_assets`.
// Keep their paths stable for downstream callers while new integrations call
// the package-local setup modules directly.
pub use crate::integrations::{
    amp::setup::{install_amp_plugin, prepare_amp_project_plugin},
    claude::setup::{
        claude_browser_mcp_config_path, claude_mcp_config_path, claude_unpeel_mcp_config_path,
        install_claude_hooks,
    },
    cline::setup::{cline_home_dir, install_cline_hooks},
    codex::setup::{install_codex_wrapper, wrapper_bin_dir},
    copilot::setup::{install_copilot_hook, prepare_copilot_project_hooks},
    cursor_agent::setup::{install_cursor_hooks, write_cursor_mcp_config},
    gemini::setup::install_gemini_hooks,
    grok::setup::{app_appearance_path, grok_appearance_bin_dir, install_grok_hooks},
    kimi::setup::{
        install_kimi_hooks, kimi_browser_mcp_config_path, kimi_global_mcp_config_path,
        kimi_mcp_config_path, kimi_unpeel_mcp_config_path,
    },
    kiro_cli::setup::install_kiro_hooks,
    muse::setup::{install_muse_hooks, muse_plugin_dir},
    opencode::setup::{install_opencode_plugin, opencode_config_dir},
};

#[cfg(test)]
pub(crate) use crate::integrations::{
    amp::setup::AMP_PLUGIN_SCRIPT,
    claude::setup::{
        build_hook_entry, claude_hook_script_path, is_stale_unpeel_claude_hook,
        prune_stale_unpeel_claude_hooks, CLAUDE_HOOK_SCRIPT, HOOK_EVENTS,
    },
    cline::setup::{write_cline_event_hook, CLINE_HOOK_SCRIPT},
    codex::setup::{
        build_codex_hook_command, build_codex_hook_entry, codex_hooks_feature_is_enabled,
        enable_codex_hooks_feature_in_toml, parse_managed_codex_hook_command,
        reconcile_codex_hooks_json, CODEX_MANAGED_HOOK_SUFFIX, CODEX_NOTIFY_NORMALIZER_SCRIPT,
        CODEX_WRAPPER_SCRIPT,
    },
    copilot::setup::COPILOT_HOOK_SCRIPT,
    cursor_agent::setup::{merge_cursor_mcp_servers_at, CURSOR_HOOK_SCRIPT},
    gemini::setup::GEMINI_HOOK_SCRIPT,
    grok::setup::{
        grok_hooks_json, GROK_COMMAND_WRAPPER_SCRIPT, GROK_DEFAULTS_WRAPPER_SCRIPT,
        GROK_HOOK_SCRIPT,
    },
    kimi::setup::{reconcile_kimi_config, upsert_kimi_code_managed_mcp, KIMI_HOOK_SCRIPT},
    kiro_cli::setup::{kiro_mcp_server_value, KIRO_HOOK_SCRIPT},
    muse::setup::{muse_plugin_manifest_json, MUSE_HOOK_SCRIPT},
    opencode::setup::OPENCODE_PLUGIN_SCRIPT,
};

#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/setup_conformance_tests.rs"
));
