use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize, with_flag,
    ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[
    ("--resume", true),
    ("--continue", false),
    ("--session-id", true),
];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, &["--session-id", "--resume"]));
    let stripped = strip_resume_flags(tokens, RESUME_FLAGS);
    match id {
        Some(id) => join(with_flag(stripped, &["--resume", &quoted(&id)])),
        None if has_resume_marker => command.trim().to_string(),
        None => join(with_flag(stripped, &["--continue"])),
    }
}

fn fresh(command: &str) -> String {
    join(strip_resume_flags(tokenize(command), RESUME_FLAGS))
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_session_id_with_resume_form() {
        assert_eq!(
            resumed("copilot --session-id old", Some("new")),
            "copilot --resume 'new'"
        );
        assert_eq!(resumed("copilot", None), "copilot --continue");
    }
}
