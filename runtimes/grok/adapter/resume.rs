use crate::resume::{
    has_resume_flag, id_in_command, join, quoted, strip_resume_flags, tokenize, uuid_flag_value,
    with_flag, NewLaunchContext, PreparedNewLaunch, ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[
    ("-r", true),
    ("--resume", true),
    ("-c", false),
    ("--continue", false),
    ("--session", true),
    ("--session-id", true),
    ("-s", true),
];
const ID_FLAGS: &[&str] = &["--session-id", "-s", "--session", "--resume", "-r"];

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
        None => join(with_flag(stripped, &["--continue"])),
    }
}

fn fresh(command: &str) -> String {
    join(strip_resume_flags(tokenize(command), RESUME_FLAGS))
}

fn prepare_new_launch(command: &str, _context: NewLaunchContext<'_>) -> PreparedNewLaunch {
    let tokens = tokenize(command);
    if has_resume_flag(&tokens, RESUME_FLAGS) {
        return PreparedNewLaunch::unchanged(command);
    }
    let id = uuid::Uuid::new_v4().to_string();
    PreparedNewLaunch {
        command: join(with_flag(tokens, &["--session-id", &quoted(&id)])),
        provider_session_id: Some(id),
        managed_storage_path: None,
    }
}

fn failure_markers(command: &str) -> Option<Vec<String>> {
    uuid_flag_value(command.trim(), &["-r", "--resume"])?;
    Some(vec!["Error: Session does not exist".to_string()])
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh)
    .with_new_launch_preparation(prepare_new_launch)
    .with_failure_markers(failure_markers);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_and_failure_marker_forms_are_exact() {
        assert_eq!(resumed("grok", None), "grok --continue");
        let id = "12345678-abcd-4ef0-8123-456789abcdef";
        assert_eq!(
            failure_markers(&format!("grok --resume '{id}'")),
            Some(vec!["Error: Session does not exist".to_string()])
        );
        assert_eq!(
            resumed("grok -r stale -c --model fast", Some("new")),
            "grok --model fast --resume 'new'"
        );
        assert_eq!(
            fresh("grok --session=legacy -s minted --continue --model fast"),
            "grok --model fast"
        );
        for command in [
            "grok -r old",
            "grok -c",
            "grok --session legacy",
            "grok -s=minted",
        ] {
            assert_eq!(
                prepare_new_launch(command, NewLaunchContext::default()),
                PreparedNewLaunch::unchanged(command),
                "must not mint over {command}"
            );
        }
    }
}
