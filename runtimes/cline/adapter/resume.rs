use crate::resume::{
    id_in_command, join, quoted, strip_resume_flags, tokenize, with_flag, ResumeAdapter,
};

const RESUME_FLAGS: &[(&str, bool)] = &[("--id", true)];

fn strip_history_subcommand(mut tokens: Vec<String>) -> Vec<String> {
    if tokens
        .get(1)
        .is_some_and(|value| value.eq_ignore_ascii_case("history"))
    {
        tokens.remove(1);
    }
    tokens
}

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let has_history_subcommand = tokens
        .get(1)
        .is_some_and(|value| value.eq_ignore_ascii_case("history"));
    let id = provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or_else(|| id_in_command(&tokens, &["--id"]));
    let stripped = strip_history_subcommand(strip_resume_flags(tokens, RESUME_FLAGS));
    match id {
        Some(id) => join(with_flag(stripped, &["--id", &quoted(&id)])),
        // Cline has no continue-last flag; its history picker is the fallback.
        None if has_history_subcommand => command.trim().to_string(),
        None => "cline history".to_string(),
    }
}

fn fresh(command: &str) -> String {
    join(strip_history_subcommand(strip_resume_flags(
        tokenize(command),
        RESUME_FLAGS,
    )))
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_exact_id_or_history_picker() {
        assert_eq!(resumed("cline", Some("c1")), "cline --id 'c1'");
        assert_eq!(resumed("cline --plan", None), "cline history");
        assert_eq!(
            resumed("cline history --plan", None),
            "cline history --plan"
        );
        assert_eq!(resumed("cline history", Some("c1")), "cline --id 'c1'");
        assert_eq!(fresh("cline history --id=old --plan"), "cline --plan");
    }
}
