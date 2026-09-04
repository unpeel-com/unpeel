use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize, with_flag,
    ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[
    ("--session", true),
    ("--resume", true),
    ("-S", true),
    ("-r", true),
    ("--continue", false),
    ("-C", false),
];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, &["--session", "-S", "--resume", "-r"]));
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
    fn canonicalizes_every_supported_id_flag() {
        assert_eq!(
            resumed("kimi --yolo -S old", Some("new")),
            "kimi --yolo --session 'new'"
        );
        assert_eq!(resumed("kimi", None), "kimi --continue");
        assert_eq!(
            resumed("kimi -r old -C --yolo", Some("new")),
            "kimi --yolo --session 'new'"
        );
        assert_eq!(fresh("kimi -C --resume=old --yolo"), "kimi --yolo");
        assert_eq!(resumed("kimi -C --yolo", None), "kimi -C --yolo");
    }
}
