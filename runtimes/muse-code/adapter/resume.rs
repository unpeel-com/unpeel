use crate::resume::{
    insert_subcommand, join, quoted, strip_resume_subcommand, tokenize, unquote, ResumeAdapter,
};

fn embedded_resume_id(tokens: &[String]) -> Option<String> {
    if tokens.get(1).is_none_or(|value| value != "resume") {
        return None;
    }
    tokens
        .get(2)
        .filter(|value| value.as_str() != "--last" && !value.starts_with('-'))
        .map(|value| unquote(value))
        .filter(|value| !value.is_empty())
}

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let embedded_id = embedded_resume_id(&tokens);
    let stripped = strip_resume_subcommand(tokens, &["resume"]);
    let subcommand = match provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or(embedded_id)
    {
        Some(id) => vec!["resume".to_string(), quoted(&id)],
        None => vec!["resume".to_string(), "--last".to_string()],
    };
    join(insert_subcommand(stripped, subcommand))
}

fn fresh(command: &str) -> String {
    join(strip_resume_subcommand(tokenize(command), &["resume"]))
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_subcommand_is_inserted_and_removed() {
        assert_eq!(resumed("muse --yolo", None), "muse resume --last --yolo");
        assert_eq!(
            resumed("muse resume 'muse-1' --yolo", None),
            "muse resume 'muse-1' --yolo"
        );
        assert_eq!(
            resumed("muse resume 'stale' --yolo", Some("fresh")),
            "muse resume 'fresh' --yolo"
        );
        assert_eq!(fresh("muse resume 'x' --yolo"), "muse --yolo");
    }
}
