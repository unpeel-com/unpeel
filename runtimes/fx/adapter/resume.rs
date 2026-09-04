use crate::resume::{
    has_resume_flag, join, quoted, strip_resume_flags, tokenize, with_flag, ResumeAdapter,
};

// Root-level resume shortcuts (cli_surface.zig usage line):
// `--resume [last|<id>]`, `--resume-last`, `--continue`, `-c`, and the `-r`
// interactive picker. `--resume` takes an optional target; declaring it
// value-taking strips an attached id with it and the helper already leaves a
// following flag alone.
const RESUME_FLAGS: &[(&str, bool)] = &[
    ("-c", false),
    ("--continue", false),
    ("--resume-last", false),
    ("-r", false),
    ("--resume", true),
];

/// The `--resume-<id>` attached form. `--resume-last` is its own flag above.
fn resume_prefix_id(token: &str) -> Option<&str> {
    let id = token.strip_prefix("--resume-")?;
    (!id.is_empty() && id != "last").then_some(id)
}

/// `fx resume [last|<id>]` / `fx session resume …` subcommand forms; returns
/// the index of the `resume` token.
fn subcommand_resume_index(tokens: &[String]) -> Option<usize> {
    match tokens.get(1).map(String::as_str) {
        Some("resume") => Some(1),
        Some("session") if tokens.get(2).map(String::as_str) == Some("resume") => Some(2),
        _ => None,
    }
}

fn has_resume_marker(tokens: &[String]) -> bool {
    has_resume_flag(tokens, RESUME_FLAGS)
        || tokens.iter().any(|token| resume_prefix_id(token).is_some())
        || subcommand_resume_index(tokens).is_some()
}

fn strip_subcommand_resume(tokens: Vec<String>) -> Vec<String> {
    let Some(resume_index) = subcommand_resume_index(&tokens) else {
        return tokens;
    };
    let mut output: Vec<String> = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if resume_index == 2 && index == 1 {
            // The `session` prefix owning this `resume`.
            index += 1;
            continue;
        }
        if index == resume_index {
            index += 1; // `resume`
            if index < tokens.len() && !tokens[index].starts_with('-') {
                index += 1; // bare `last` / `<id>` target
            }
            if tokens.get(index).map(String::as_str) == Some("--id") {
                index += 1;
                if index < tokens.len() && !tokens[index].starts_with('-') {
                    index += 1;
                }
            }
            continue;
        }
        output.push(tokens[index].clone());
        index += 1;
    }
    output
}

// fx has no hooks, so a provider session id is normally unknown and resume
// falls back to the documented workspace-scoped continue-last (`--continue`
// resumes the latest session for the cwd, which is the Session's project or
// worktree). A command that already carries any resume marker is exact by
// construction and stays untouched.
fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    if has_resume_marker(&tokens) {
        return command.trim().to_string();
    }
    match provider_session_id.filter(|id| !id.is_empty()) {
        Some(id) => join(with_flag(tokens, &["--resume", &quoted(id)])),
        None => join(with_flag(tokens, &["--continue"])),
    }
}

fn fresh(command: &str) -> String {
    let tokens = strip_subcommand_resume(tokenize(command));
    let tokens: Vec<String> = tokens
        .into_iter()
        .filter(|token| resume_prefix_id(token).is_none())
        .collect();
    join(strip_resume_flags(tokens, RESUME_FLAGS))
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_launch_resumes_via_documented_continue_last() {
        assert_eq!(resumed("fx", None), "fx --continue");
        assert_eq!(resumed("fx --record", None), "fx --record --continue");
        assert_eq!(resumed("fx", Some("abc123")), "fx --resume 'abc123'");
    }

    #[test]
    fn existing_resume_markers_stay_exact() {
        for command in [
            "fx -c",
            "fx --continue",
            "fx --resume-last",
            "fx --resume abc123",
            "fx --resume-abc123",
            "fx -r",
            "fx resume last",
            "fx resume abc123",
            "fx resume --id abc123",
            "fx session resume last",
        ] {
            assert_eq!(resumed(command, None), command, "must not double-resume");
            assert_eq!(
                resumed(command, Some("other")),
                command,
                "an explicit marker outranks a captured id"
            );
        }
    }

    #[test]
    fn fresh_strips_every_resume_form() {
        assert_eq!(fresh("fx -c"), "fx");
        assert_eq!(fresh("fx --continue --record"), "fx --record");
        assert_eq!(fresh("fx --resume-last"), "fx");
        assert_eq!(fresh("fx --resume abc123"), "fx");
        assert_eq!(fresh("fx --resume-abc123"), "fx");
        assert_eq!(fresh("fx resume last"), "fx");
        assert_eq!(fresh("fx resume abc123 --record"), "fx --record");
        assert_eq!(fresh("fx resume --id abc123 --record"), "fx --record");
        assert_eq!(fresh("fx session resume --id abc123"), "fx");
        // A bare `--resume` must not eat an unrelated following flag.
        assert_eq!(fresh("fx --resume --record"), "fx --record");
    }
}
