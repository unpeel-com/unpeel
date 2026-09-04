//! Host-owned observation of an agent started inside an existing terminal.
//!
//! This module deliberately observes only the live foreground job. A match can
//! improve presentation and activity handling, but it must never rewrite the
//! Session's launch command or imply that the process can be relaunched or
//! resumed. Process aliases and package-path signatures come from the runtime
//! catalog; observations continue to publish the compatibility `legacy_slug`
//! until the Host/Controller protocol migrates to stable runtime IDs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum process arguments retained as observation evidence.
///
/// Matching uses the complete kernel snapshot, but persisted evidence stops at
/// the argument that established the match and is bounded again here. This
/// avoids copying a provider prompt or arbitrary later CLI arguments into the
/// Session manifest.
const MAX_OBSERVED_ARGV_ITEMS: usize = 8;
const MAX_OBSERVED_ARGV_BYTES: usize = 1_024;
const PID_START_TOLERANCE_MS: u64 = 10_000;

/// A volatile, display-safe observation of the foreground agent runtime.
///
/// `PartialEq`/`Eq` are load-bearing: the Host compares observations and only
/// writes a manifest when the foreground identity actually changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRuntimeObservation {
    /// Transitional built-in integration ID (for example `claude`).
    #[serde(rename = "id")]
    pub runtime_id: String,
    /// Diagnostics are additive: an older/cooperative writer may know the
    /// runtime identity without being able to expose a host PID or name.
    #[serde(default)]
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started_at: Option<u64>,
    /// Kernel process group that owned the terminal when this runtime was
    /// observed. Resume Agent retains this identity after the job is stopped
    /// or backgrounded and will not treat the shell as safe until both the
    /// exact PID/start identity and every member of this group are gone.
    #[serde(default, rename = "processGroupID", alias = "processGroupId")]
    pub process_group_id: u32,
    #[serde(default)]
    pub process_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForegroundProcess {
    pid: u32,
    parent_pid: u32,
    process_group_id: u32,
    started_at_ms: u64,
    name: String,
    argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForegroundJob {
    process_group_id: u32,
    processes: Vec<ForegroundProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchStrength {
    Wrapper,
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeMatch {
    runtime_id: &'static str,
    /// Number of argv cells needed to explain the match. Later cells may hold
    /// a user prompt and are intentionally not persisted.
    evidence_argv_len: usize,
    strength: MatchStrength,
}

/// Fresh process evidence used by the Host before it submits a same-PTY
/// Resume Agent command. Foreground PGID ownership alone is insufficient: a
/// shell can regain the terminal while its stopped agent remains in a
/// background job, and `exec` preserves the session leader PID while
/// replacing the shell executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedSessionProcessInspection {
    pub shell_executable_matches: bool,
    pub shell_invocation_matches: bool,
    /// Best catalog-recognized runtime anywhere in the owned kernel session,
    /// not only the runtime named by the Session's stable command. A user can
    /// start a different agent and background it before asking to resume the
    /// managed one; that must still block injection.
    pub recognized_runtime_observation: Option<ActiveRuntimeObservation>,
    /// Whether the exact last observation (PID/start and its isolated process
    /// group) is still present. `None` means the prior identity was incomplete
    /// and disappearance could not be proven, so callers must fail closed.
    pub prior_runtime_present: Option<bool>,
}

/// Verify the session leader's executable and inspect every process still in
/// its kernel session, including stopped/background groups. Returns `None`
/// when the process set or owner identity cannot be proven; callers must fail
/// closed. The leader start time and executable are checked again after the
/// scan to close PID-reuse and concurrent-`exec` windows as far as the kernel
/// APIs permit.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn inspect_owned_session_processes(
    session_leader_pid: u32,
    session_leader_started_at_ms: u64,
    expected_shell_executable: &Path,
    expected_runtime_id: &str,
    prior_runtime_observation: Option<&ActiveRuntimeObservation>,
) -> Option<OwnedSessionProcessInspection> {
    if session_leader_pid <= 1
        || expected_runtime_id.trim().is_empty()
        || !pid_start_matches(session_leader_pid, session_leader_started_at_ms)
    {
        return None;
    }
    let expected_shell = canonical_executable(expected_shell_executable)?;
    let shell_before = platform::process_executable_path(session_leader_pid)
        .and_then(|path| canonical_executable(&path));
    let processes = platform::session_processes(session_leader_pid)?;
    let shell_after = platform::process_executable_path(session_leader_pid)
        .and_then(|path| canonical_executable(&path));
    if !pid_start_matches(session_leader_pid, session_leader_started_at_ms) {
        return None;
    }

    let shell_executable_matches = shell_before.as_ref() == Some(&expected_shell)
        && shell_after.as_ref() == Some(&expected_shell);
    let shell_invocation_matches = processes
        .iter()
        .find(|process| process.pid == session_leader_pid)
        .and_then(|process| process.argv.as_deref())
        .is_some_and(interactive_login_shell_argv);
    let recognized_runtime_observation = processes
        .iter()
        .filter_map(|process| {
            let runtime_match = match_process(process)?;
            Some((
                runtime_match
                    .runtime_id
                    .eq_ignore_ascii_case(expected_runtime_id),
                runtime_match.strength,
                u32::MAX.saturating_sub(process.pid),
                observation(process, runtime_match),
            ))
        })
        // Prefer the Session's expected runtime when multiple recognized jobs
        // exist, then the strongest/stablest catalog evidence. Any result is
        // still a blocker; this preference only makes diagnostics useful.
        .max_by_key(|(expected, strength, stable_pid_order, _)| {
            (*expected, *strength, *stable_pid_order)
        })
        .map(|(_, _, _, observation)| observation);
    let prior_runtime_present = prior_runtime_observation
        .map(|prior| prior_runtime_present_in_processes(&processes, prior, session_leader_pid))
        .unwrap_or(Some(false));
    Some(OwnedSessionProcessInspection {
        shell_executable_matches,
        shell_invocation_matches,
        recognized_runtime_observation,
        prior_runtime_present,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn inspect_owned_session_processes(
    _session_leader_pid: u32,
    _session_leader_started_at_ms: u64,
    _expected_shell_executable: &Path,
    _expected_runtime_id: &str,
    _prior_runtime_observation: Option<&ActiveRuntimeObservation>,
) -> Option<OwnedSessionProcessInspection> {
    None
}

/// A fresh catalog miss is not proof that a previously observed job exited:
/// the same process can `exec` an unrecognized binary, change its argv/title,
/// or leave children in its old process group. Require both identities gone.
fn prior_runtime_present_in_processes(
    processes: &[ForegroundProcess],
    prior: &ActiveRuntimeObservation,
    session_leader_pid: u32,
) -> Option<bool> {
    let prior_started_at = prior.pid_started_at?;
    if prior.pid <= 1 || prior.process_group_id <= 1 {
        return None;
    }
    let exact_identity_present = processes.iter().any(|process| {
        process.pid == prior.pid
            && process.started_at_ms.abs_diff(prior_started_at) <= PID_START_TOLERANCE_MS
    });
    // Initial managed commands may run in the noninteractive startup shell's
    // own PGID. That group intentionally survives as the final login shell,
    // so only an isolated runtime job group can be required to disappear.
    let process_group_present = prior.process_group_id != session_leader_pid
        && processes
            .iter()
            .any(|process| process.process_group_id == prior.process_group_id);
    Some(exact_identity_present || process_group_present)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn canonical_executable(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// The Host's persistent shell is always launched login+interactive and never
/// with a command string. Checking this in addition to the executable path
/// distinguishes it from `exec zsh -c ...` / `exec bash -c ...`, which keep
/// the same PID, start time, session, PGID, and executable.
fn interactive_login_shell_argv(argv: &[String]) -> bool {
    let Some(argv0) = argv.first() else {
        return false;
    };
    let argv0_is_login = argv0
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name.starts_with('-'));
    let mut login = argv0_is_login;
    let mut interactive = false;

    for option in argv.iter().skip(1) {
        match option.as_str() {
            "--login" => login = true,
            "--interactive" => interactive = true,
            "--command" | "-c" => return false,
            "--" => return false,
            _ if option.starts_with("--") => return false,
            _ if option.starts_with('-') => {
                for flag in option.trim_start_matches('-').chars() {
                    match flag {
                        'l' => login = true,
                        'i' => interactive = true,
                        'c' => return false,
                        // The Host supplies only login/interactive flags. An
                        // unexpected mode is not the shell invocation it owns.
                        _ => return false,
                    }
                }
            }
            _ => return false,
        }
    }

    login && (interactive || (argv0_is_login && argv.len() == 1))
}

/// Inspect a PTY's current foreground process group for a supported runtime.
///
/// `session_leader_pid` is the child PID returned by `portable-pty` (which is
/// also the session leader after its `setsid`), and
/// `session_leader_started_at_ms` must be the kernel start time captured when
/// that child was spawned. The observer fails closed if the owner PID was
/// recycled, if the group is outside that session, or if process identity
/// changes while argv is being read.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn observe_foreground_runtime(
    session_leader_pid: u32,
    session_leader_started_at_ms: u64,
    foreground_process_group_id: i32,
) -> Option<ActiveRuntimeObservation> {
    if session_leader_pid <= 1 || foreground_process_group_id <= 1 {
        return None;
    }
    if !pid_start_matches(session_leader_pid, session_leader_started_at_ms) {
        return None;
    }

    let process_group_id = u32::try_from(foreground_process_group_id).ok()?;
    let job = platform::foreground_job(
        session_leader_pid,
        process_group_id,
        session_leader_started_at_ms,
    )?;

    // Close the observation's PID-reuse window. A caller may keep the child
    // object alive, but this public boundary does not assume that it did.
    if !pid_start_matches(session_leader_pid, session_leader_started_at_ms) {
        return None;
    }

    identify_runtime_in_job(&job)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn observe_foreground_runtime(
    _session_leader_pid: u32,
    _session_leader_started_at_ms: u64,
    _foreground_process_group_id: i32,
) -> Option<ActiveRuntimeObservation> {
    None
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn pid_start_matches(pid: u32, expected_ms: u64) -> bool {
    crate::session_host::process_start_time_ms(pid)
        .is_some_and(|actual_ms| actual_ms.abs_diff(expected_ms) <= PID_START_TOLERANCE_MS)
}

fn identify_runtime_in_job(job: &ForegroundJob) -> Option<ActiveRuntimeObservation> {
    // The process-group leader is authoritative when it is itself a runtime.
    // This prevents a short-lived child whose executable happens to look like
    // another agent from stealing the terminal's identity.
    if let Some(leader) = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)
    {
        if let Some(runtime_match) = match_process(leader) {
            return Some(observation(leader, runtime_match));
        }
    }

    let mut best: Option<(MatchStrength, usize, u32, &ForegroundProcess, RuntimeMatch)> = None;
    for process in &job.processes {
        let Some(runtime_match) = match_process(process) else {
            continue;
        };
        let depth =
            ancestry_depth(process.pid, job.process_group_id, &job.processes).unwrap_or(usize::MAX);
        let candidate = (
            runtime_match.strength,
            // Prefer a process nearest the foreground group leader after
            // match strength. Reverse the depth for tuple comparison below.
            usize::MAX.saturating_sub(depth),
            u32::MAX.saturating_sub(process.pid),
            process,
            runtime_match,
        );
        if best.as_ref().is_none_or(|current| {
            (candidate.0, candidate.1, candidate.2) > (current.0, current.1, current.2)
        }) {
            best = Some(candidate);
        }
    }

    best.map(|(_, _, _, process, runtime_match)| observation(process, runtime_match))
}

fn observation(
    process: &ForegroundProcess,
    runtime_match: RuntimeMatch,
) -> ActiveRuntimeObservation {
    // Wrapper argv can contain environment assignments or an inline shell
    // command. Those cells are useful transient matching input but are not
    // safe manifest evidence: values may be credentials and command text may
    // be a user prompt. Keep argv only for a direct executable match.
    let argv = (runtime_match.strength == MatchStrength::Direct)
        .then(|| bounded_evidence_argv(process.argv.as_deref(), runtime_match.evidence_argv_len))
        .flatten();
    ActiveRuntimeObservation {
        runtime_id: runtime_match.runtime_id.to_string(),
        pid: process.pid,
        pid_started_at: Some(process.started_at_ms),
        process_group_id: process.process_group_id,
        process_name: normalized_executable_name(&process.name),
        argv,
    }
}

fn ancestry_depth(pid: u32, ancestor: u32, processes: &[ForegroundProcess]) -> Option<usize> {
    let mut current = pid;
    for depth in 0..=processes.len() {
        if current == ancestor {
            return Some(depth);
        }
        let process = processes
            .iter()
            .find(|candidate| candidate.pid == current)?;
        if process.parent_pid == 0 || process.parent_pid == current {
            return None;
        }
        current = process.parent_pid;
    }
    None
}

fn match_process(process: &ForegroundProcess) -> Option<RuntimeMatch> {
    // Prefer the kernel process title: Node CLIs commonly replace it with the
    // CLI name. Then consider argv[0], which preserves a direct executable
    // path even when the kernel name is a generic runtime.
    if let Some(runtime_id) = runtime_id_for_executable(&process.name) {
        return Some(RuntimeMatch {
            runtime_id,
            evidence_argv_len: process.argv.as_ref().map_or(0, |_| 1),
            strength: MatchStrength::Direct,
        });
    }
    if let Some(argv) = process.argv.as_deref() {
        if let Some(runtime_id) = argv.first().and_then(|arg| runtime_id_for_executable(arg)) {
            return Some(RuntimeMatch {
                runtime_id,
                evidence_argv_len: 1,
                strength: MatchStrength::Direct,
            });
        }
        if let Some((runtime_id, evidence_argv_len)) = runtime_from_wrapper_argv(argv) {
            return Some(RuntimeMatch {
                runtime_id,
                evidence_argv_len,
                strength: MatchStrength::Wrapper,
            });
        }
    }
    // Installed Unpeel Apps are consulted only after every built-in attempt
    // has failed: built-in identity is reserved, and `app_runtime` already
    // dropped colliding or shell/wrapper aliases at index build. An App match
    // is identity/presentation data — it grants no Busy authority, installs
    // no hooks, and never enables Resume Agent (its id matches no catalog
    // runtime).
    if let Some(app) = crate::app_runtime::app_for_executable(&process.name) {
        return Some(RuntimeMatch {
            runtime_id: app.app_id,
            evidence_argv_len: process.argv.as_ref().map_or(0, |_| 1),
            strength: MatchStrength::Direct,
        });
    }
    if let Some(app) = process
        .argv
        .as_deref()
        .and_then(|argv| argv.first())
        .and_then(|arg| crate::app_runtime::app_for_executable(arg))
    {
        return Some(RuntimeMatch {
            runtime_id: app.app_id,
            evidence_argv_len: 1,
            strength: MatchStrength::Direct,
        });
    }
    None
}

/// Catalog-declared aliases, accepted only as complete executable basenames
/// after common platform suffixes are removed.
fn runtime_id_for_executable(executable: &str) -> Option<&'static str> {
    let executable = normalized_executable_name(executable);
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_process_alias_for_current_platform(&executable)
        .map(|runtime| runtime.legacy_slug.as_str())
}

fn runtime_from_wrapper_argv(argv: &[String]) -> Option<(&'static str, usize)> {
    let wrapper = normalized_executable_name(argv.first()?);
    match wrapper.as_str() {
        "node" | "bun" => runtime_from_script_argv(argv, &["-e", "--eval", "-p", "--print"]),
        name if is_python_runtime(name) => runtime_from_script_argv(argv, &["-c", "-m"]),
        "sh" | "bash" | "zsh" | "fish" => runtime_from_shell_argv(argv),
        "env" | "command" => runtime_from_prefix_argv(argv),
        "npx" | "bunx" => runtime_from_package_runner_argv(argv),
        _ => None,
    }
}

fn runtime_from_script_argv(
    argv: &[String],
    ambiguous_execution_flags: &[&str],
) -> Option<(&'static str, usize)> {
    let mut index = 1;
    while index < argv.len() {
        let arg = argv[index].as_str();
        if arg == "--" {
            index += 1;
            return argv
                .get(index)
                .and_then(|path| runtime_id_from_script_path(path))
                .map(|runtime_id| (runtime_id, index + 1));
        }
        if ambiguous_execution_flags.iter().any(|flag| {
            arg == *flag
                || (flag.starts_with("--") && arg.starts_with(&format!("{flag}=")))
                || (!flag.starts_with("--") && arg.starts_with(flag) && arg.len() > flag.len())
        }) {
            return None;
        }
        if arg.starts_with('-') {
            if runtime_option_takes_value(arg) {
                index += 1;
            }
            index += 1;
            continue;
        }
        return runtime_id_from_script_path(arg).map(|runtime_id| (runtime_id, index + 1));
    }
    None
}

fn runtime_from_shell_argv(argv: &[String]) -> Option<(&'static str, usize)> {
    let command_index =
        argv.iter().enumerate().skip(1).find_map(|(index, arg)| {
            matches!(arg.as_str(), "-c" | "--command").then_some(index + 1)
        })?;
    let command = argv.get(command_index)?;
    let token = first_shell_token(command)?;
    runtime_id_for_executable(token).map(|runtime_id| (runtime_id, command_index + 1))
}

fn runtime_from_prefix_argv(argv: &[String]) -> Option<(&'static str, usize)> {
    for (index, arg) in argv.iter().enumerate().skip(1) {
        if arg == "--" || arg.starts_with('-') || is_safe_env_assignment(arg) {
            continue;
        }
        return runtime_id_for_executable(arg).map(|runtime_id| (runtime_id, index + 1));
    }
    None
}

fn runtime_from_package_runner_argv(argv: &[String]) -> Option<(&'static str, usize)> {
    for (index, arg) in argv.iter().enumerate().skip(1) {
        if arg == "--" || arg.starts_with('-') {
            continue;
        }
        return runtime_id_from_script_path(arg).map(|runtime_id| (runtime_id, index + 1));
    }
    None
}

fn runtime_id_from_script_path(path: &str) -> Option<&'static str> {
    if let Some(runtime_id) = runtime_id_for_executable(path) {
        return Some(runtime_id);
    }

    let components = path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .map(normalized_executable_name)
        .collect::<Vec<_>>();
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_script_path_components_for_current_platform(&components)
        .map(|runtime| runtime.legacy_slug.as_str())
}

pub(crate) fn normalized_executable_name(executable: &str) -> String {
    let basename = executable
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(executable)
        .trim_matches(|character| matches!(character, '\'' | '"'))
        .trim_start_matches('-')
        .to_ascii_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".ps1", ".js"] {
        if let Some(stripped) = basename.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    basename
}

fn is_python_runtime(name: &str) -> bool {
    name == "python"
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

fn runtime_option_takes_value(option: &str) -> bool {
    matches!(
        option,
        "-r" | "--require"
            | "--loader"
            | "--import"
            | "--experimental-loader"
            | "--inspect-port"
            | "-W"
            | "-X"
            | "-S"
            | "-L"
            | "-o"
    )
}

fn first_shell_token(command: &str) -> Option<&str> {
    let command = command.trim_start();
    let first = command.chars().next()?;
    if matches!(first, '\'' | '"') {
        let start = first.len_utf8();
        let end = command[start..]
            .find(first)
            .map(|offset| start + offset)
            .unwrap_or(command.len());
        return command.get(start..end).filter(|token| !token.is_empty());
    }
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    command.get(..end).filter(|token| !token.is_empty())
}

fn is_safe_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn bounded_evidence_argv(argv: Option<&[String]>, evidence_len: usize) -> Option<Vec<String>> {
    let argv = argv?;
    let mut remaining = MAX_OBSERVED_ARGV_BYTES;
    let mut bounded = Vec::new();
    for arg in argv.iter().take(evidence_len.min(MAX_OBSERVED_ARGV_ITEMS)) {
        if remaining == 0 {
            break;
        }
        let mut end = arg.len().min(remaining);
        while !arg.is_char_boundary(end) {
            end -= 1;
        }
        bounded.push(arg[..end].to_string());
        remaining = remaining.saturating_sub(end);
    }
    (!bounded.is_empty()).then_some(bounded)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{ForegroundJob, ForegroundProcess};

    const PROC_PGRP_ONLY: u32 = 2;

    pub(super) fn foreground_job(
        session_leader_pid: u32,
        process_group_id: u32,
        _session_leader_started_at_ms: u64,
    ) -> Option<ForegroundJob> {
        let mut processes = Vec::new();
        for pid in process_group_pids(process_group_id) {
            let Some(info) = process_bsdinfo(pid) else {
                continue;
            };
            if info.pbi_pgid != process_group_id
                || unsafe { libc::getsid(pid as libc::pid_t) } != session_leader_pid as libc::pid_t
            {
                continue;
            }

            let Some(started_at_ms) = crate::session_host::process_start_time_ms(pid) else {
                continue;
            };
            let Some(name) = process_name(&info) else {
                continue;
            };
            let argv = process_argv(pid);
            if crate::session_host::process_start_time_ms(pid) != Some(started_at_ms) {
                continue;
            }
            processes.push(ForegroundProcess {
                pid,
                parent_pid: info.pbi_ppid,
                process_group_id: info.pbi_pgid,
                started_at_ms,
                name,
                argv,
            });
        }
        (!processes.is_empty()).then_some(ForegroundJob {
            process_group_id,
            processes,
        })
    }

    pub(super) fn session_processes(session_leader_pid: u32) -> Option<Vec<ForegroundProcess>> {
        let mut processes = Vec::new();
        for pid in all_pids()? {
            let Some(info) = process_bsdinfo(pid) else {
                continue;
            };
            if unsafe { libc::getsid(pid as libc::pid_t) } != session_leader_pid as libc::pid_t {
                continue;
            }
            let Some(started_at_ms) = crate::session_host::process_start_time_ms(pid) else {
                continue;
            };
            let Some(name) = process_name(&info) else {
                continue;
            };
            let argv = process_argv(pid);
            if crate::session_host::process_start_time_ms(pid) != Some(started_at_ms) {
                continue;
            }
            processes.push(ForegroundProcess {
                pid,
                parent_pid: info.pbi_ppid,
                process_group_id: info.pbi_pgid,
                started_at_ms,
                name,
                argv,
            });
        }
        processes
            .iter()
            .any(|process| process.pid == session_leader_pid)
            .then_some(processes)
    }

    pub(super) fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let length = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buffer.as_mut_ptr() as *mut libc::c_void,
                buffer.len() as u32,
            )
        };
        if length <= 0 {
            return None;
        }
        buffer.truncate(length as usize);
        Some(std::path::PathBuf::from(
            String::from_utf8_lossy(&buffer).into_owned(),
        ))
    }

    fn all_pids() -> Option<Vec<u32>> {
        let estimated = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if estimated <= 0 {
            return None;
        }
        let mut capacity = (estimated as usize).saturating_add(64);
        for _ in 0..6 {
            let mut pids = vec![0 as libc::pid_t; capacity];
            let buffer_bytes = pids.len().checked_mul(std::mem::size_of::<libc::pid_t>())?;
            let returned = unsafe {
                libc::proc_listallpids(
                    pids.as_mut_ptr() as *mut libc::c_void,
                    i32::try_from(buffer_bytes).ok()?,
                )
            };
            if returned <= 0 {
                return None;
            }
            let returned = returned as usize;
            if returned < capacity {
                pids.truncate(returned);
                return Some(
                    pids.into_iter()
                        .filter_map(|pid| u32::try_from(pid).ok())
                        .filter(|pid| *pid > 1)
                        .collect(),
                );
            }
            capacity = capacity.saturating_mul(2);
        }
        None
    }

    fn process_group_pids(process_group_id: u32) -> Vec<u32> {
        let mut capacity = 16usize;
        for _ in 0..8 {
            let mut pids = vec![0 as libc::pid_t; capacity];
            let buffer_bytes = pids.len() * std::mem::size_of::<libc::pid_t>();
            let returned_bytes = unsafe {
                libc::proc_listpids(
                    PROC_PGRP_ONLY,
                    process_group_id,
                    pids.as_mut_ptr() as *mut libc::c_void,
                    buffer_bytes as libc::c_int,
                )
            };
            if returned_bytes <= 0 {
                return Vec::new();
            }
            let returned_bytes = returned_bytes as usize;
            let count = returned_bytes / std::mem::size_of::<libc::pid_t>();
            if returned_bytes < buffer_bytes {
                pids.truncate(count);
                return pids
                    .into_iter()
                    .filter_map(|pid| u32::try_from(pid).ok())
                    .filter(|pid| *pid > 1)
                    .collect();
            }
            capacity = capacity.saturating_mul(2);
        }
        Vec::new()
    }

    fn process_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let returned = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr() as *mut libc::c_void,
                size,
            )
        };
        (returned == size).then(|| unsafe { info.assume_init() })
    }

    fn process_name(info: &libc::proc_bsdinfo) -> Option<String> {
        let end = info
            .pbi_comm
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(info.pbi_comm.len());
        (end > 0).then(|| {
            let bytes = info.pbi_comm[..end]
                .iter()
                .map(|byte| *byte as u8)
                .collect::<Vec<_>>();
            String::from_utf8_lossy(&bytes).into_owned()
        })
    }

    fn process_argv(pid: u32) -> Option<Vec<String>> {
        let buffer = kern_procargs2(pid)?;
        if buffer.len() < 4 {
            return None;
        }
        let argc = i32::from_ne_bytes(buffer[..4].try_into().ok()?);
        if argc < 1 {
            return None;
        }

        let rest = &buffer[4..];
        let executable_end = rest.iter().position(|byte| *byte == 0)?;
        let mut cursor = executable_end;
        while cursor < rest.len() && rest[cursor] == 0 {
            cursor += 1;
        }

        let mut argv = Vec::with_capacity(argc as usize);
        for _ in 0..argc {
            let suffix = rest.get(cursor..)?;
            let end = suffix.iter().position(|byte| *byte == 0)?;
            if end == 0 {
                return None;
            }
            argv.push(String::from_utf8_lossy(&suffix[..end]).into_owned());
            cursor = cursor.checked_add(end + 1)?;
        }
        Some(argv)
    }

    fn kern_procargs2(pid: u32) -> Option<Vec<u8>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size: libc::size_t = 0;
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || size == 0
        {
            return None;
        }

        let mut buffer = vec![0; size];
        if unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            return None;
        }
        buffer.truncate(size);
        Some(buffer)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{ForegroundJob, ForegroundProcess};
    use std::collections::{HashSet, VecDeque};

    #[derive(Debug)]
    struct ProcStat {
        parent_pid: u32,
        process_group_id: u32,
        session_id: u32,
        name: String,
    }

    pub(super) fn foreground_job(
        session_leader_pid: u32,
        process_group_id: u32,
        _session_leader_started_at_ms: u64,
    ) -> Option<ForegroundJob> {
        let mut processes = Vec::new();
        for pid in process_tree_pids([session_leader_pid, process_group_id]) {
            let Some(stat) = process_stat(pid) else {
                continue;
            };
            if stat.process_group_id != process_group_id || stat.session_id != session_leader_pid {
                continue;
            }
            let Some(started_at_ms) = crate::session_host::process_start_time_ms(pid) else {
                continue;
            };
            let argv = process_argv(pid);
            if crate::session_host::process_start_time_ms(pid) != Some(started_at_ms) {
                continue;
            }
            processes.push(ForegroundProcess {
                pid,
                parent_pid: stat.parent_pid,
                process_group_id: stat.process_group_id,
                started_at_ms,
                name: stat.name,
                argv,
            });
        }
        (!processes.is_empty()).then_some(ForegroundJob {
            process_group_id,
            processes,
        })
    }

    pub(super) fn session_processes(session_leader_pid: u32) -> Option<Vec<ForegroundProcess>> {
        let mut processes = Vec::new();
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let pid = entry.file_name().to_string_lossy().parse::<u32>().ok();
            let Some(pid) = pid.filter(|pid| *pid > 1) else {
                continue;
            };
            let Some(stat) = process_stat(pid) else {
                continue;
            };
            if stat.session_id != session_leader_pid {
                continue;
            }
            let Some(started_at_ms) = crate::session_host::process_start_time_ms(pid) else {
                continue;
            };
            let argv = process_argv(pid);
            if crate::session_host::process_start_time_ms(pid) != Some(started_at_ms) {
                continue;
            }
            processes.push(ForegroundProcess {
                pid,
                parent_pid: stat.parent_pid,
                process_group_id: stat.process_group_id,
                started_at_ms,
                name: stat.name,
                argv,
            });
        }
        processes
            .iter()
            .any(|process| process.pid == session_leader_pid)
            .then_some(processes)
    }

    pub(super) fn process_executable_path(pid: u32) -> Option<std::path::PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    fn process_tree_pids(roots: impl IntoIterator<Item = u32>) -> Vec<u32> {
        let mut pending = VecDeque::new();
        let mut visited = HashSet::new();
        for pid in roots {
            if pid > 1 && visited.insert(pid) {
                pending.push_back(pid);
            }
        }

        let mut pids = Vec::new();
        while let Some(pid) = pending.pop_front() {
            pids.push(pid);
            for task_id in process_task_ids(pid) {
                for child_pid in process_task_children(pid, task_id) {
                    if child_pid > 1 && visited.insert(child_pid) {
                        pending.push_back(child_pid);
                    }
                }
            }
        }
        pids
    }

    fn process_task_ids(pid: u32) -> Vec<u32> {
        std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect()
    }

    fn process_task_children(pid: u32, task_id: u32) -> Vec<u32> {
        std::fs::read_to_string(format!("/proc/{pid}/task/{task_id}/children"))
            .ok()
            .into_iter()
            .flat_map(|children| {
                children
                    .split_whitespace()
                    .filter_map(|child| child.parse().ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn process_stat(pid: u32) -> Option<ProcStat> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_process_stat(&stat)
    }

    fn parse_process_stat(stat: &str) -> Option<ProcStat> {
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let name = stat.get(open + 1..close)?.to_string();
        let fields = stat
            .get(close + 2..)?
            .split_whitespace()
            .collect::<Vec<_>>();
        Some(ProcStat {
            parent_pid: fields.get(1)?.parse().ok()?,
            process_group_id: fields.get(2)?.parse().ok()?,
            session_id: fields.get(3)?.parse().ok()?,
            name,
        })
    }

    fn process_argv(pid: u32) -> Option<Vec<String>> {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let argv = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>();
        (!argv.is_empty()).then_some(argv)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stat_parser_handles_names_with_spaces_and_parens() {
            let parsed = parse_process_stat("123 (name with ) paren) S 7 456 789 0 456")
                .expect("valid stat");
            assert_eq!(parsed.name, "name with ) paren");
            assert_eq!(parsed.parent_pid, 7);
            assert_eq!(parsed.process_group_id, 456);
            assert_eq!(parsed.session_id, 789);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_shell_argv_requires_login_interactive_without_a_command() {
        let argv = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };

        assert!(interactive_login_shell_argv(&argv(&[
            "/bin/bash",
            "-l",
            "-i"
        ])));
        assert!(interactive_login_shell_argv(&argv(&["-zsh"])));
        assert!(interactive_login_shell_argv(&argv(&["-fish", "-i"])));

        assert!(!interactive_login_shell_argv(&argv(&[
            "/bin/bash",
            "-l",
            "-i",
            "-c",
            "sleep 300"
        ])));
        assert!(!interactive_login_shell_argv(&argv(&[
            "/bin/zsh",
            "-lic",
            "sleep 300"
        ])));
        assert!(!interactive_login_shell_argv(&argv(&[
            "/bin/bash",
            "-c",
            "sleep 300"
        ])));
        assert!(!interactive_login_shell_argv(&argv(&["/bin/bash"])));
    }

    fn process(pid: u32, parent_pid: u32, name: &str, argv: &[&str]) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            parent_pid,
            process_group_id: pid,
            started_at_ms: 123_000 + u64::from(pid),
            name: name.to_string(),
            argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
        }
    }

    fn job(process_group_id: u32, processes: Vec<ForegroundProcess>) -> ForegroundJob {
        ForegroundJob {
            process_group_id,
            processes,
        }
    }

    #[test]
    fn direct_names_cover_every_current_builtin() {
        for runtime in
            crate::runtime_catalog::builtin_runtime_catalog().current_platform_descriptors()
        {
            for name in &runtime.detection.process_aliases {
                assert_eq!(
                    runtime_id_for_executable(name),
                    Some(runtime.legacy_slug.as_str()),
                    "{name}"
                );
                assert_eq!(
                    runtime_id_for_executable(&format!("/usr/local/bin/{name}")),
                    Some(runtime.legacy_slug.as_str()),
                    "path: {name}"
                );
            }
        }
    }

    #[test]
    fn plain_shell_and_unrelated_commands_stay_generic() {
        for name in ["zsh", "bash", "node", "python3", "git", "cargo"] {
            assert_eq!(runtime_id_for_executable(name), None, "{name}");
        }
    }

    #[test]
    fn node_package_paths_recognize_claude_and_codex() {
        let claude = process(
            42,
            1,
            "node",
            &[
                "/usr/local/bin/node",
                "/opt/node_modules/@anthropic-ai/claude-code/cli.js",
                "--dangerously-skip-permissions",
            ],
        );
        let codex = process(
            43,
            1,
            "node",
            &[
                "/usr/local/bin/node",
                "/opt/node_modules/@openai/codex/bin/codex.js",
                "--full-auto",
            ],
        );
        assert_eq!(match_process(&claude).unwrap().runtime_id, "claude");
        assert_eq!(match_process(&codex).unwrap().runtime_id, "codex");
    }

    #[test]
    fn foreground_group_leader_wins_over_nested_candidate() {
        let observation = identify_runtime_in_job(&job(
            10,
            vec![
                process(10, 1, "claude", &["claude"]),
                process(11, 10, "codex", &["codex"]),
            ],
        ))
        .expect("runtime");
        assert_eq!(observation.runtime_id, "claude");
        assert_eq!(observation.pid, 10);
    }

    #[test]
    fn direct_child_beats_a_more_distant_wrapped_match() {
        let observation = identify_runtime_in_job(&job(
            10,
            vec![
                process(10, 1, "zsh", &["zsh"]),
                process(11, 10, "codex", &["codex"]),
                process(
                    12,
                    11,
                    "node",
                    &["node", "/x/@anthropic-ai/claude-code/cli.js"],
                ),
            ],
        ))
        .expect("runtime");
        assert_eq!(observation.runtime_id, "codex");
    }

    #[test]
    fn observation_evidence_does_not_persist_provider_prompt() {
        let observation = identify_runtime_in_job(&job(
            10,
            vec![process(
                10,
                1,
                "claude",
                &["claude", "private user prompt", "--flag"],
            )],
        ))
        .expect("runtime");
        assert_eq!(observation.argv, Some(vec!["claude".to_string()]));
    }

    #[test]
    fn serialized_shape_is_stable_and_camel_case() {
        let observation = ActiveRuntimeObservation {
            runtime_id: "claude".to_string(),
            pid: 42,
            pid_started_at: Some(1234),
            process_group_id: 40,
            process_name: "claude".to_string(),
            argv: Some(vec!["claude".to_string()]),
        };
        assert_eq!(
            serde_json::to_value(observation).unwrap(),
            serde_json::json!({
                "id": "claude",
                "pid": 42,
                "pidStartedAt": 1234,
                "processGroupID": 40,
                "processName": "claude",
                "argv": ["claude"]
            })
        );

        let identity_only: ActiveRuntimeObservation =
            serde_json::from_value(serde_json::json!({ "id": "claude" }))
                .expect("additive diagnostics remain optional");
        assert_eq!(identity_only.runtime_id, "claude");
        assert_eq!(identity_only.pid, 0);
        assert_eq!(identity_only.process_group_id, 0);
        assert!(identity_only.process_name.is_empty());
    }

    #[test]
    fn prior_runtime_identity_or_group_must_be_definitively_gone() {
        let prior = ActiveRuntimeObservation {
            runtime_id: "pi".to_string(),
            pid: 42,
            pid_started_at: Some(123_042),
            process_group_id: 42,
            process_name: "pi".to_string(),
            argv: Some(vec!["pi".to_string()]),
        };
        // The observed PID can exec/rename to something the catalog no longer
        // recognizes. PID/start identity alone must retain the blocker.
        let renamed = process(42, 1, "sleep", &["sleep", "300"]);
        assert_eq!(
            prior_runtime_present_in_processes(&[renamed], &prior, 10),
            Some(true)
        );

        // The leader can exit while a child remains in its old process group.
        let mut group_child = process(43, 1, "sleep", &["sleep", "300"]);
        group_child.process_group_id = 42;
        assert_eq!(
            prior_runtime_present_in_processes(&[group_child], &prior, 10),
            Some(true)
        );

        let unrelated = process(99, 1, "sleep", &["sleep", "300"]);
        assert_eq!(
            prior_runtime_present_in_processes(&[unrelated], &prior, 10),
            Some(false)
        );

        let mut incomplete = prior;
        incomplete.process_group_id = 0;
        assert_eq!(
            prior_runtime_present_in_processes(&[], &incomplete, 10),
            None
        );

        // The persistent shell itself remains in the startup PGID after the
        // runtime child exits; that shared group cannot be a permanent block.
        let mut shared_group = ActiveRuntimeObservation {
            process_group_id: 10,
            ..incomplete
        };
        shared_group.pid = 42;
        shared_group.pid_started_at = Some(123_042);
        let shell = process(10, 1, "bash", &["-bash"]);
        assert_eq!(
            prior_runtime_present_in_processes(&[shell], &shared_group, 10),
            Some(false)
        );
    }

    #[test]
    fn safe_prefixes_are_detected_without_evaluating_shell_text() {
        let env = vec![
            "env".to_string(),
            "TOKEN=secret".to_string(),
            "claude".to_string(),
        ];
        assert_eq!(runtime_from_wrapper_argv(&env), Some(("claude", 3)));
        let observation = observation(
            &process(10, 1, "env", &["env", "TOKEN=secret", "claude"]),
            match_process(&process(10, 1, "env", &["env", "TOKEN=secret", "claude"]))
                .expect("wrapped runtime"),
        );
        let serialized = serde_json::to_string(&observation).expect("serialized observation");
        assert!(!serialized.contains("secret"));
        assert_eq!(observation.argv, None);
        let compound = vec![
            "zsh".to_string(),
            "-c".to_string(),
            "foo && claude".to_string(),
        ];
        assert_eq!(runtime_from_wrapper_argv(&compound), None);
    }
}
