use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize, with_flag,
    ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[
    ("-c", false),
    ("--continue", false),
    ("-s", true),
    ("--session", true),
];
const ID_FLAGS: &[&str] = &["-s", "--session"];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, ID_FLAGS));
    let stripped = strip_resume_flags(tokens, RESUME_FLAGS);
    match id {
        Some(id) => join(with_flag(stripped, &["--session", &quoted(&id)])),
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
    fn uses_session_or_continue() {
        assert_eq!(resumed("opencode", Some("s1")), "opencode --session 's1'");
        assert_eq!(resumed("opencode", None), "opencode --continue");
        assert_eq!(
            resumed("opencode -s old -c --model fast", Some("new")),
            "opencode --model fast --session 'new'"
        );
        assert_eq!(
            fresh("opencode -c --session=old --model fast"),
            "opencode --model fast"
        );
    }
}
