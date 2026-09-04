use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "claude",
    file_backed: true,
    collect_document: None,
    collect_line,
    resume_id_from_command,
    trusted_roots,
    path_matches,
    find_by_id,
    find_best,
    title_candidate: Some(summary_title_candidate),
    model_from_value: None,
};

fn resume_id_from_command(command: &str) -> Option<String> {
    flag_value(&shell_words(command), &["--resume", "-r"])
}

fn trusted_roots() -> Vec<PathBuf> {
    claude_projects_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
}

fn claude_projects_root() -> Option<PathBuf> {
    Some(user_home_dir()?.join(".claude").join("projects"))
}

fn claude_project_dir(cwd: &str) -> Option<PathBuf> {
    Some(claude_projects_root()?.join(cwd.replace('/', "-")))
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let dir = claude_project_dir(cwd)?;
    let direct = dir.join(format!("{provider_id}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    list_files_with_extensions(&dir, &["jsonl"])
        .into_iter()
        .find(|path| file_name_contains(path, provider_id))
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    best_file_for_session(
        list_files_with_extensions(&claude_project_dir(&manifest.cwd)?, &["jsonl"]),
        manifest.session.created_at,
    )
}

fn summary_title_candidate(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("summary") {
            continue;
        }
        if let Some(title) = value
            .get("summary")
            .and_then(Value::as_str)
            .and_then(crate::session_host::normalize_prompt_title)
        {
            return Some(title);
        }
    }
    None
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    match value.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let Some(content) = value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            else {
                return;
            };
            let mut text = Vec::new();
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(value) = block.get("text").and_then(Value::as_str) {
                            text.push(value);
                        }
                    }
                    Some("thinking") if include_tools => {
                        if let Some(value) = block.get("thinking").and_then(Value::as_str) {
                            push_transcript_entry(
                                &mut state.entries,
                                "Reasoning",
                                value.to_string(),
                            );
                        }
                    }
                    Some("tool_use") if include_tools => {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        let call_id = block.get("id").and_then(Value::as_str);
                        if let Some(id) = call_id {
                            state.tool_names.insert(id.to_string(), name.to_string());
                            state.tool_inputs.insert(id.to_string(), input.clone());
                        }
                        if !maybe_push_file_change_tool(&mut state.entries, call_id, name, &input) {
                            push_tool_call_entry(
                                &mut state.entries,
                                call_id,
                                name,
                                summarize_tool_input(name, &input),
                                metadata_from_tool_input(
                                    &input,
                                    &["file_path", "path", "cmd", "command"],
                                ),
                            );
                        }
                    }
                    _ => {}
                }
            }
            push_transcript_entry(&mut state.entries, "Assistant", text.join("\n"));
        }
        Some("user") => {
            let Some(message) = value.get("message") else {
                return;
            };
            if let Some(content) = message.get("content").and_then(Value::as_str) {
                push_user_transcript_entry(&mut state.entries, content);
                return;
            }
            let Some(content) = message.get("content").and_then(Value::as_array) else {
                return;
            };
            let mut user_text = Vec::new();
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            user_text.push(text);
                        }
                    }
                    Some("tool_result") if include_tools => {
                        let tool_id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let name = state
                            .tool_names
                            .get(tool_id)
                            .map(String::as_str)
                            .unwrap_or("tool");
                        if let Some(text) = text_from_content(block.get("content")) {
                            push_tool_result_entry(
                                &mut state.entries,
                                (!tool_id.is_empty()).then_some(tool_id),
                                name,
                                &text,
                                None,
                                HashMap::new(),
                            );
                        }
                    }
                    _ => {}
                }
            }
            push_user_transcript_entry(&mut state.entries, &user_text.join("\n"));
        }
        _ => {}
    }
}
