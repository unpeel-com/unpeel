use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "omp",
    file_backed: true,
    collect_document: None,
    collect_line,
    resume_id_from_command,
    trusted_roots,
    path_matches,
    find_by_id,
    find_best,
    title_candidate: Some(session_title_candidate),
    model_from_value: None,
};

fn resume_id_from_command(command: &str) -> Option<String> {
    flag_value(&shell_words(command), &["--resume", "-r"])
}

/// OMP writes one `<timestamp>_<session id>.jsonl` per conversation below the
/// agent directory, next to the profile's other state. The agent directory
/// follows `PI_CODING_AGENT_DIR`, the same override the installer honors.
fn omp_sessions_root() -> Option<PathBuf> {
    Some(crate::integrations::omp::setup::omp_agent_dir()?.join("sessions"))
}

fn trusted_roots() -> Vec<PathBuf> {
    omp_sessions_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
}

/// OMP groups conversations by working directory: the home prefix is stripped
/// and every separator becomes `-`. Task and subagent logs sit in a nested
/// directory named after their owning session, so only top-level files are
/// conversation transcripts.
fn omp_project_dir(cwd: &str) -> Option<PathBuf> {
    let root = omp_sessions_root()?;
    let relative = user_home_dir()
        .and_then(|home| Path::new(cwd).strip_prefix(home).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| Path::new(cwd).to_path_buf());
    Some(root.join(format!("-{}", relative.to_string_lossy().replace('/', "-"))))
}

fn session_files(root: &Path, cwd: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = omp_project_dir(cwd) {
        if dir.is_dir() {
            files.extend(list_files_with_extensions(&dir, &["jsonl"]));
        }
    }
    if files.is_empty() {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    files.extend(list_files_with_extensions(&path, &["jsonl"]));
                }
            }
        }
    }
    files
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let root = omp_sessions_root()?;
    session_files(&root, cwd)
        .into_iter()
        .find(|path| file_name_contains(path, provider_id))
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    let root = omp_sessions_root()?;
    best_file_for_session(
        session_files(&root, &manifest.cwd),
        manifest.session.created_at,
    )
}

/// OMP records the generated conversation title as its own `title` entry.
fn session_title_candidate(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("title") {
            continue;
        }
        if let Some(title) = value
            .get("title")
            .and_then(Value::as_str)
            .and_then(crate::session_host::normalize_prompt_title)
        {
            return Some(title);
        }
    }
    None
}

/// OMP persists `{type:"message", message:{role, content}}` entries; assistant
/// content mixes `text`, `thinking`, and `toolCall` blocks, and every tool call
/// gets a separate `toolResult` entry.
fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    let Some(message) = value.get("message") else {
        return;
    };
    match message.get("role").and_then(Value::as_str) {
        Some("assistant") => {
            let Some(content) = message.get("content").and_then(Value::as_array) else {
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
                    Some("toolCall") if include_tools => {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = block.get("arguments").cloned().unwrap_or(Value::Null);
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
                                    &["path", "file_path", "cmd", "command"],
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
            let Some(content) = message.get("content") else {
                return;
            };
            if let Some(content) = content.as_str() {
                push_user_transcript_entry(&mut state.entries, content);
                return;
            }
            let Some(content) = content.as_array() else {
                return;
            };
            let mut user_text = Vec::new();
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        user_text.push(text);
                    }
                }
            }
            push_user_transcript_entry(&mut state.entries, &user_text.join("\n"));
        }
        Some("toolResult") if include_tools => {
            let call_id = message
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = message
                .get("toolName")
                .and_then(Value::as_str)
                .or_else(|| state.tool_names.get(call_id).map(String::as_str))
                .unwrap_or("tool");
            if let Some(text) = text_from_content(message.get("content")) {
                push_tool_result_entry(
                    &mut state.entries,
                    (!call_id.is_empty()).then_some(call_id),
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
