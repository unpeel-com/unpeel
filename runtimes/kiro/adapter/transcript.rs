use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "kiro-cli",
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
    flag_value(&shell_words(command), &["--resume-id"])
}

fn kiro_home_dir() -> Option<PathBuf> {
    let path = std::env::var_os("KIRO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home_dir().unwrap_or_default().join(".kiro"));
    if path.is_absolute() {
        Some(path)
    } else {
        Some(std::env::current_dir().ok()?.join(path))
    }
}

fn kiro_sessions_root() -> Option<PathBuf> {
    Some(kiro_home_dir()?.join("sessions"))
}

pub(super) fn kiro_project_dir(cwd: &str) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let canonical = fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let cwd_hash = format!("{digest:x}");
    Some(kiro_sessions_root()?.join(&cwd_hash[..16]))
}

fn trusted_roots() -> Vec<PathBuf> {
    kiro_sessions_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
        && path.file_name().is_some_and(|name| {
            name == "messages.jsonl" || name.to_string_lossy().ends_with(".jsonl")
        })
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let v3 = kiro_project_dir(cwd)?
        .join(provider_id)
        .join("messages.jsonl");
    if v3.is_file() {
        return Some(v3);
    }
    let v2 = kiro_sessions_root()?
        .join("cli")
        .join(format!("{provider_id}.jsonl"));
    v2.is_file().then_some(v2)
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    let mut candidates = walk_files_with_extensions(
        &kiro_project_dir(&manifest.cwd)?,
        &["jsonl"],
        PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
    )
    .into_iter()
    .filter(|path| {
        path.file_name()
            .is_some_and(|name| name == "messages.jsonl")
    })
    .collect::<Vec<_>>();

    if let Some(cli) = kiro_sessions_root().map(|root| root.join("cli")) {
        candidates.extend(
            list_files_with_extensions(&cli, &["jsonl"])
                .into_iter()
                .filter(|path| v2_transcript_cwd(path).as_deref() == Some(manifest.cwd.as_str())),
        );
    }
    best_file_for_session(candidates, manifest.session.created_at)
}

fn v2_transcript_cwd(path: &Path) -> Option<String> {
    let metadata = path.with_extension("json");
    let value: Value = serde_json::from_slice(&fs::read(metadata).ok()?).ok()?;
    value.get("cwd")?.as_str().map(str::to_string)
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    if let Some(payload) = value.get("payload") {
        match payload.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(text) = text_from_content(payload.get("content")) {
                    push_user_transcript_entry(&mut state.entries, &text);
                }
            }
            Some("assistant") => {
                if let Some(text) = text_from_content(payload.get("content")) {
                    push_transcript_entry(&mut state.entries, "Assistant", text);
                }
            }
            Some("tool_call") if include_tools => {
                let call_id = payload.get("toolCallId").and_then(Value::as_str);
                let name = payload
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let input = payload.get("args").cloned().unwrap_or(Value::Null);
                if let Some(id) = call_id {
                    state.tool_names.insert(id.to_string(), name.to_string());
                }
                if !maybe_push_file_change_tool(&mut state.entries, call_id, name, &input) {
                    push_tool_call_entry(
                        &mut state.entries,
                        call_id,
                        name,
                        summarize_tool_input(name, &input),
                        metadata_from_tool_input(&input, &["file_path", "path", "cmd", "command"]),
                    );
                }
            }
            Some("tool_result") if include_tools => {
                let call_id = payload.get("toolCallId").and_then(Value::as_str);
                let name = call_id
                    .and_then(|id| state.tool_names.get(id))
                    .map(String::as_str)
                    .unwrap_or("tool");
                if let Some(output) = text_from_content(payload.get("content")) {
                    let status = payload
                        .get("success")
                        .and_then(Value::as_bool)
                        .map(|success| if success { "success" } else { "error" }.to_string());
                    push_tool_result_entry(
                        &mut state.entries,
                        call_id,
                        name,
                        &output,
                        status,
                        HashMap::new(),
                    );
                }
            }
            _ => {}
        }
        return;
    }

    let Some(data) = value.get("data") else {
        return;
    };
    let content = data
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    match value.get("kind").and_then(Value::as_str) {
        Some("Prompt") => {
            let text = content
                .iter()
                .filter(|item| item.get("kind").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("data").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() {
                push_user_transcript_entry(&mut state.entries, &text);
            }
        }
        Some("AssistantMessage") => {
            let text = content
                .iter()
                .filter(|item| item.get("kind").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("data").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.trim().is_empty() {
                push_transcript_entry(&mut state.entries, "Assistant", text);
            }
            if include_tools {
                for item in content
                    .iter()
                    .filter(|item| item.get("kind").and_then(Value::as_str) == Some("toolUse"))
                {
                    let tool = item.get("data").unwrap_or(&Value::Null);
                    let call_id = tool.get("toolUseId").and_then(Value::as_str);
                    let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = tool.get("input").cloned().unwrap_or(Value::Null);
                    if let Some(id) = call_id {
                        state.tool_names.insert(id.to_string(), name.to_string());
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
            }
        }
        Some("ToolResults") if include_tools => {
            for item in content
                .iter()
                .filter(|item| item.get("kind").and_then(Value::as_str) == Some("toolResult"))
            {
                let result = item.get("data").unwrap_or(&Value::Null);
                let call_id = result.get("toolUseId").and_then(Value::as_str);
                let name = call_id
                    .and_then(|id| state.tool_names.get(id))
                    .map(String::as_str)
                    .unwrap_or("tool");
                let output = result
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.get("data"))
                    .map(|data| {
                        data.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| data.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !output.trim().is_empty() {
                    push_tool_result_entry(
                        &mut state.entries,
                        call_id,
                        name,
                        &output,
                        result
                            .get("status")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        HashMap::new(),
                    );
                }
            }
        }
        _ => {}
    }
}
