use crate::resume::{insert_subcommand, join, quoted, tokenize, unquote, ResumeAdapter};

fn continue_shape(tokens: &[String]) -> Option<(usize, usize)> {
    let first = tokens.get(1)?.to_ascii_lowercase();
    if ["continue", "c"].contains(&first.as_str()) {
        return Some((1, 2));
    }
    if !["threads", "thread", "t"].contains(&first.as_str()) {
        return None;
    }
    let second = tokens.get(2)?.to_ascii_lowercase();
    ["continue", "c"]
        .contains(&second.as_str())
        .then_some((2, 3))
}

fn embedded_continue_id(tokens: &[String]) -> Option<String> {
    let (_, target_index) = continue_shape(tokens)?;
    tokens
        .get(target_index)
        .filter(|value| value.as_str() != "--last" && !value.starts_with('-'))
        .map(|value| unquote(value))
        .filter(|value| !value.is_empty())
}

fn strip_continue(tokens: Vec<String>) -> Vec<String> {
    let Some((last_subcommand_index, target_index)) = continue_shape(&tokens) else {
        return tokens;
    };
    let mut remainder_index = last_subcommand_index + 1;
    if tokens
        .get(target_index)
        .is_some_and(|target| target == "--last" || !target.starts_with('-'))
    {
        remainder_index = target_index + 1;
    }
    let mut output = vec![tokens[0].clone()];
    output.extend(tokens.into_iter().skip(remainder_index));
    output
}

fn resumed(command: &str, provider_session_id: Option<&str>) -> String {
    let tokens = tokenize(command);
    let embedded_id = embedded_continue_id(&tokens);
    let stripped = strip_continue(tokens);
    let mut subcommand = vec!["threads".to_string(), "continue".to_string()];
    match provider_session_id
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .or(embedded_id)
    {
        Some(id) => subcommand.push(quoted(&id)),
        None => subcommand.push("--last".to_string()),
    }
    join(insert_subcommand(stripped, subcommand))
}

fn fresh(command: &str) -> String {
    join(strip_continue(tokenize(command)))
}

pub(super) const ADAPTER: ResumeAdapter = ResumeAdapter::new(resumed, fresh);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_continue_is_idempotent() {
        assert_eq!(resumed("amp", Some("th1")), "amp threads continue 'th1'");
        assert_eq!(
            resumed("amp threads continue --last", None),
            "amp threads continue --last"
        );
        assert_eq!(
            resumed("amp threads continue 'thread-1' --profile fast", None),
            "amp threads continue 'thread-1' --profile fast"
        );
        assert_eq!(
            resumed("amp continue legacy-1 --profile fast", None),
            "amp threads continue 'legacy-1' --profile fast"
        );
        assert_eq!(
            resumed("amp t c stale --profile fast", Some("fresh")),
            "amp threads continue 'fresh' --profile fast"
        );
        assert_eq!(
            fresh("amp thread continue old --profile fast"),
            "amp --profile fast"
        );
    }
}
