use std::path::Path;

pub fn command_head(command: &str) -> &str {
    let head = command.split_whitespace().next().unwrap_or_default();
    Path::new(head)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(head)
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

pub fn shell_name(shell_path: &str) -> String {
    Path::new(shell_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell_path)
        .to_ascii_lowercase()
}
