use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize, with_flag,
    ResumeAdapter,
};

/// OMP's own conversation flags: `-r/--resume <id prefix|path>`, `-c/--continue`.
const RESUME_FLAGS: &[(&str, bool)] = &[
    ("-c", false),
    ("--continue", false),
    ("-r", true),
    ("--resume", true),
];
const ID_FLAGS: &[&str] = &["--resume", "-r"];

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_resume_marker = has_resume_flag(&tokens, RESUME_FLAGS);
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, ID_FLAGS));
    let stripped = strip_resume_flags(tokens, RESUME_FLAGS);
    match id {
        Some(id) => join(with_flag(stripped, &["--resume", &quoted(&id)])),
        None if has_resume_marker => command.trim().to_string(),
        // New launches run `omp` as typed, so continue-last is OMP's own
        // most-recent conversation.
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
    fn captured_session_id_becomes_a_resume_flag() {
        assert_eq!(
            resumed("omp", Some("01a0b32e-ef83-72d6-9210-d5cd55a1523c")),
            "omp --resume '01a0b32e-ef83-72d6-9210-d5cd55a1523c'"
        );
    }

    #[test]
    fn without_a_captured_id_resume_falls_back_to_continue_last() {
        assert_eq!(resumed("omp", None), "omp --continue");
        assert_eq!(resumed("omp --auto-approve", None), "omp --auto-approve --continue");
    }

    #[test]
    fn an_existing_resume_flag_is_replaced_not_duplicated() {
        assert_eq!(
            resumed("omp --resume old-id", Some("new-id")),
            "omp --resume 'new-id'"
        );
        // An explicit continue marker is already the continue-last intent.
        assert_eq!(resumed("omp -c", None), "omp -c");
        // A marker that carries an id is normalized onto the long flag.
        assert_eq!(resumed("omp -r old-id", None), "omp --resume 'old-id'");
    }

    #[test]
    fn fresh_launch_drops_every_conversation_flag() {
        assert_eq!(fresh("omp --resume abc"), "omp");
        assert_eq!(fresh("omp --continue"), "omp");
        assert_eq!(fresh("omp -r abc --auto-approve"), "omp --auto-approve");
    }
}
