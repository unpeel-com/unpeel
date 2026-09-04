use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_leading_subcommands, strip_resume_flags,
    tokenize, with_flag, ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[("--resume", true), ("--continue", false)];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_subcommand = tokens
        .get(1)
        .is_some_and(|value| value.eq_ignore_ascii_case("resume"));
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, &["--resume"]));
    let stripped = strip_leading_subcommands(strip_resume_flags(tokens, RESUME_FLAGS), &["resume"]);
    match id {
        Some(id) => join(with_flag(stripped, &["--resume", &quoted(&id)])),
        None if has_resume_subcommand || has_resume_marker => command.trim().to_string(),
        None => join(with_flag(stripped, &["--continue"])),
    }
}

fn fresh(command: &str) -> String {
    join(strip_leading_subcommands(
        strip_resume_flags(tokenize(command), RESUME_FLAGS),
        &["resume"],
    ))
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_resume_id() {
        assert_eq!(
            resumed("cursor-agent --resume old --force", Some("new")),
            "cursor-agent --force --resume 'new'"
        );
        assert_eq!(resumed("cursor-agent", None), "cursor-agent --continue");
        assert_eq!(
            resumed("cursor-agent resume old --force", None),
            "cursor-agent resume old --force"
        );
        assert_eq!(
            resumed("cursor-agent resume old --force", Some("new")),
            "cursor-agent --force --resume 'new'"
        );
        assert_eq!(
            fresh("cursor-agent resume old --force --continue"),
            "cursor-agent --force"
        );
    }
}
