use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize, with_flag,
    ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[
    ("-r", true),
    ("--resume", true),
    ("--resume-id", true),
    ("--resume-picker", false),
];
const ID_FLAGS: &[&str] = &["--resume-id", "--resume", "-r"];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, ID_FLAGS));
    let stripped = strip_resume_flags(tokens, RESUME_FLAGS);
    match id {
        Some(id) => join(with_flag(stripped, &["--resume-id", &quoted(&id)])),
        None if has_resume_marker => command.trim().to_string(),
        None => join(with_flag(stripped, &["--resume"])),
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
    fn exact_id_and_picker_forms_are_preserved() {
        assert_eq!(
            resumed("kiro-cli --v3", Some("k1")),
            "kiro-cli --v3 --resume-id 'k1'"
        );
        assert_eq!(resumed("kiro-cli --v3", None), "kiro-cli --v3 --resume");
        assert_eq!(
            resumed("kiro-cli -r old --v3", Some("new")),
            "kiro-cli --v3 --resume-id 'new'"
        );
        assert_eq!(
            fresh("kiro-cli --resume-picker --resume-id=old --v3"),
            "kiro-cli --v3"
        );
        assert_eq!(
            resumed("kiro-cli --resume-picker --v3", None),
            "kiro-cli --resume-picker --v3"
        );
    }
}
