use super::*;

const TRANSCRIPT_HEAD_BYTES: u64 = 256 * 1024;
const TRANSCRIPT_SEARCH_LIMIT: usize = 800;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "codex",
    file_backed: true,
    collect_document: None,
    collect_line,
    resume_id_from_command,
    trusted_roots,
    path_matches,
    find_by_id,
    find_best,
    title_candidate: None,
    model_from_value: None,
};

fn resume_id_from_command(command: &str) -> Option<String> {
    let words = shell_words(command);
    let resume_index = words.iter().position(|word| word == "resume")?;
    words[resume_index + 1..]
        .iter()
        .find(|word| !word.starts_with('-'))
        .cloned()
}

fn codex_sessions_root() -> Option<PathBuf> {
    Some(user_home_dir()?.join(".codex").join("sessions"))
}

fn trusted_roots() -> Vec<PathBuf> {
    codex_sessions_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
}

fn find_by_id(_cwd: &str, provider_id: &str) -> Option<PathBuf> {
    walk_files_with_extensions(&codex_sessions_root()?, &["jsonl"], TRANSCRIPT_SEARCH_LIMIT)
        .into_iter()
        .find(|path| file_name_contains(path, provider_id))
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    best_file_for_session(
        walk_files_with_extensions(&codex_sessions_root()?, &["jsonl"], TRANSCRIPT_SEARCH_LIMIT)
            .into_iter()
            .filter(|path| transcript_cwd(path).as_deref() == Some(manifest.cwd.as_str()))
            .collect(),
        manifest.session.created_at,
    )
}

fn transcript_cwd(path: &Path) -> Option<String> {
    let head = read_head_utf8(path, TRANSCRIPT_HEAD_BYTES).ok()?;
    for line in head.lines().take(20) {
        let value = serde_json::from_str::<Value>(line).ok()?;
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        if let Some(cwd) = value
            .get("payload")
            .and_then(|payload| payload.get("cwd"))
            .and_then(Value::as_str)
        {
            return Some(cwd.to_string());
        }
    }
    None
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    match value.get("type").and_then(Value::as_str) {
        Some("event_msg") => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("user_message") => {
                    if let Some(message) = payload.get("message").and_then(Value::as_str) {
                        push_user_transcript_entry(&mut state.entries, message);
                    }
                }
                Some("agent_message") => {
                    if let Some(message) = payload.get("message").and_then(Value::as_str) {
                        push_transcript_entry(&mut state.entries, "Assistant", message.to_string());
                    }
                }
                _ => {}
            }
        }
        Some("response_item") => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let role = match payload.get("role").and_then(Value::as_str) {
                        Some("user") => "User",
                        Some("assistant") => "Assistant",
                        _ => return,
                    };
                    if let Some(text) = text_from_content(payload.get("content")) {
                        if role == "User" {
                            push_user_transcript_entry(&mut state.entries, &text);
                        } else {
                            push_transcript_entry(&mut state.entries, role, text);
                        }
                    }
                }
                Some("function_call") | Some("custom_tool_call") if include_tools => {
                    let name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let call_id = payload
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let input = tool_input(payload);
                    if !call_id.is_empty() {
                        state
                            .tool_names
                            .insert(call_id.to_string(), name.to_string());
                        state.tool_inputs.insert(call_id.to_string(), input.clone());
                    }
                    let call_id = (!call_id.is_empty()).then_some(call_id);
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
                Some("function_call_output") | Some("custom_tool_call_output") if include_tools => {
                    let call_id = payload
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = state
                        .tool_names
                        .get(call_id)
                        .map(String::as_str)
                        .unwrap_or("tool");
                    if let Some(output) = payload.get("output").and_then(Value::as_str) {
                        push_tool_result_entry(
                            &mut state.entries,
                            (!call_id.is_empty()).then_some(call_id),
                            name,
                            output,
                            None,
                            HashMap::new(),
                        );
                    }
                }
                Some("patch_apply_end") if include_tools => {
                    push_patch_apply_end(&mut state.entries, payload);
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn tool_input(payload: &Value) -> Value {
    if let Some(arguments) = payload.get("arguments").and_then(Value::as_str) {
        return serde_json::from_str(arguments).unwrap_or_else(|_| json!({ "content": arguments }));
    }
    if let Some(input) = payload.get("input") {
        if input.is_string() {
            return json!({ "content": input.as_str().unwrap_or_default() });
        }
        return input.clone();
    }
    Value::Null
}

fn push_patch_apply_end(entries: &mut Vec<TranscriptEntry>, payload: &Value) {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            payload
                .get("success")
                .and_then(Value::as_bool)
                .map(|success| if success { "success" } else { "failed" }.to_string())
        });
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let Some(changes) = payload.get("changes").and_then(Value::as_object) else {
        return;
    };
    for (path, change) in changes {
        let operation = change
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("patch");
        let mut metadata = HashMap::new();
        if let Some(move_path) = change.get("move_path").and_then(Value::as_str) {
            metadata.insert("movePath".to_string(), move_path.to_string());
        }
        let diff = change
            .get("unified_diff")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(diff) = diff.as_deref() {
            let (additions, deletions) = diff_line_counts(diff);
            metadata.insert("additions".to_string(), additions.to_string());
            metadata.insert("deletions".to_string(), deletions.to_string());
        }
        push_file_change_entry(
            entries,
            call_id.map(|id| format!("{id}:{path}")),
            Some("apply_patch".to_string()),
            Some(path.to_string()),
            operation,
            diff,
            status.clone(),
            metadata,
        );
    }
}
