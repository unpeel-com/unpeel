use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "kimi",
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
    flag_value(
        &shell_words(command),
        &["--session", "--resume", "-S", "-r"],
    )
}

fn kimi_share_dir() -> Option<PathBuf> {
    let path = std::env::var_os("KIMI_SHARE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home_dir().unwrap_or_default().join(".kimi"));
    if path.is_absolute() {
        Some(path)
    } else {
        Some(std::env::current_dir().ok()?.join(path))
    }
}

fn kimi_sessions_root() -> Option<PathBuf> {
    Some(kimi_share_dir()?.join("sessions"))
}

fn kimi_code_home_dir() -> Option<PathBuf> {
    let path = std::env::var_os("KIMI_CODE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home_dir().unwrap_or_default().join(".kimi-code"));
    if path.is_absolute() {
        Some(path)
    } else {
        Some(std::env::current_dir().ok()?.join(path))
    }
}

fn kimi_code_sessions_root() -> Option<PathBuf> {
    Some(kimi_code_home_dir()?.join("sessions"))
}

fn trusted_roots() -> Vec<PathBuf> {
    [kimi_sessions_root(), kimi_code_sessions_root()]
        .into_iter()
        .flatten()
        .collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
        && path.file_name().is_some_and(|name| {
            name == "context.jsonl"
                || name == "wire.jsonl"
                || name.to_string_lossy().ends_with(".jsonl")
        })
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    if let Some(path) = code_indexed_transcripts(Some(provider_id), Some(cwd))
        .into_iter()
        .next()
        .or_else(|| find_code_transcript_by_id(provider_id))
    {
        return Some(path);
    }
    let project = project_dir(cwd)?;
    let direct = project.join(provider_id).join("context.jsonl");
    if direct.is_file() {
        return Some(direct);
    }
    let legacy = project.join(format!("{provider_id}.jsonl"));
    legacy.is_file().then_some(legacy)
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    let mut candidates = code_indexed_transcripts(None, Some(&manifest.cwd));
    if let Some(project) = project_dir(&manifest.cwd) {
        candidates.extend(context_files(&project));
    }
    best_file_for_session(candidates, manifest.session.created_at)
}

fn code_indexed_transcripts(provider_id: Option<&str>, cwd: Option<&str>) -> Vec<PathBuf> {
    let Some(home) = kimi_code_home_dir() else {
        return Vec::new();
    };
    let Some(sessions_root) = kimi_code_sessions_root() else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(home.join("session_index.jsonl")) else {
        return Vec::new();
    };

    let mut index = HashMap::<String, (PathBuf, String)>::new();
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session_id) = value.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        if value.get("deleted").and_then(Value::as_bool) == Some(true) {
            index.remove(session_id);
            continue;
        }
        let (Some(session_dir), Some(work_dir)) = (
            value.get("sessionDir").and_then(Value::as_str),
            value.get("workDir").and_then(Value::as_str),
        ) else {
            continue;
        };
        let session_dir = PathBuf::from(session_dir);
        if session_dir.file_name().and_then(|name| name.to_str()) != Some(session_id)
            || !session_dir.is_absolute()
        {
            continue;
        }
        index.insert(session_id.to_string(), (session_dir, work_dir.to_string()));
    }

    let requested_cwd = cwd.map(|value| {
        fs::canonicalize(value)
            .unwrap_or_else(|_| PathBuf::from(value))
            .to_string_lossy()
            .to_string()
    });
    index
        .into_iter()
        .filter(|(session_id, _)| provider_id.is_none_or(|wanted| wanted == session_id))
        .filter_map(|(_, (session_dir, indexed_cwd))| {
            let state_cwd = fs::read(session_dir.join("state.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
                .and_then(|value| {
                    value
                        .get("workDir")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or(indexed_cwd);
            let state_cwd = fs::canonicalize(&state_cwd)
                .unwrap_or_else(|_| PathBuf::from(&state_cwd))
                .to_string_lossy()
                .to_string();
            if requested_cwd
                .as_ref()
                .is_some_and(|wanted| wanted != &state_cwd)
            {
                return None;
            }
            let transcript = session_dir.join("agents").join("main").join("wire.jsonl");
            (transcript.is_file() && path_within_root(&transcript, &sessions_root))
                .then_some(transcript)
        })
        .collect()
}

fn find_code_transcript_by_id(provider_id: &str) -> Option<PathBuf> {
    let root = kimi_code_sessions_root()?;
    walk_files_with_extensions(&root, &["jsonl"], PROVIDER_TRANSCRIPT_SEARCH_LIMIT)
        .into_iter()
        .find(|path| {
            path.file_name().is_some_and(|name| name == "wire.jsonl")
                && path
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some(provider_id)
        })
}

pub(super) fn project_dir(cwd: &str) -> Option<PathBuf> {
    let canonical = fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    let canonical = canonical.to_string_lossy();
    let cwd_hash = format!("{:x}", md5::compute(canonical.as_bytes()));
    Some(kimi_sessions_root()?.join(cwd_hash))
}

fn context_files(project: &Path) -> Vec<PathBuf> {
    fs::read_dir(project)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                let context = path.join("context.jsonl");
                return context.is_file().then_some(context);
            }
            (path.is_file() && has_extension(&path, &["jsonl"])).then_some(path)
        })
        .collect()
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    if value.get("type").and_then(Value::as_str) == Some("context.append_message") {
        let Some(message) = value.get("message") else {
            return;
        };
        if message.get("role").and_then(Value::as_str) == Some("user")
            && message
                .pointer("/origin/kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind != "user")
        {
            return;
        }
        collect_line(message, include_tools, state);
        return;
    }
    if value.get("type").and_then(Value::as_str) == Some("context.append_loop_event") {
        collect_code_loop_event(
            value.get("event").unwrap_or(&Value::Null),
            include_tools,
            state,
        );
        return;
    }

    match value.get("role").and_then(Value::as_str) {
        Some("user") => {
            if let Some(text) = text_content(value.get("content"), false) {
                push_user_transcript_entry(&mut state.entries, &text);
            }
        }
        Some("assistant") => {
            if include_tools {
                if let Some(reasoning) = reasoning_content(value.get("content")) {
                    push_transcript_entry(&mut state.entries, "Reasoning", reasoning);
                }
            }
            if let Some(text) = text_content(value.get("content"), true) {
                push_transcript_entry(&mut state.entries, "Assistant", text);
            }
            if include_tools {
                for call in value
                    .get("tool_calls")
                    .or_else(|| value.get("toolCalls"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let call_id = call.get("id").and_then(Value::as_str);
                    let function = call.get("function").unwrap_or(call);
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let input = match function.get("arguments") {
                        Some(Value::String(raw)) => {
                            serde_json::from_str(raw).unwrap_or_else(|_| json!({ "content": raw }))
                        }
                        Some(value) => value.clone(),
                        None => Value::Null,
                    };
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
        Some("tool") if include_tools => {
            let call_id = value
                .get("tool_call_id")
                .or_else(|| value.get("toolCallId"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = state
                .tool_names
                .get(call_id)
                .map(String::as_str)
                .unwrap_or("tool");
            if let Some(output) = text_content(value.get("content"), true) {
                push_tool_result_entry(
                    &mut state.entries,
                    (!call_id.is_empty()).then_some(call_id),
                    name,
                    &output,
                    None,
                    HashMap::new(),
                );
            }
        }
        _ => {}
    }
}

fn collect_code_loop_event(event: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    match event.get("type").and_then(Value::as_str) {
        Some("content.part") => {
            let part = event.get("part").unwrap_or(&Value::Null);
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.trim().is_empty() {
                            push_transcript_entry(
                                &mut state.entries,
                                "Assistant",
                                text.trim().to_string(),
                            );
                        }
                    }
                }
                Some("think" | "thinking") if include_tools => {
                    let reasoning = part
                        .get("think")
                        .or_else(|| part.get("thinking"))
                        .and_then(Value::as_str);
                    if let Some(reasoning) = reasoning.filter(|text| !text.trim().is_empty()) {
                        push_transcript_entry(
                            &mut state.entries,
                            "Reasoning",
                            reasoning.trim().to_string(),
                        );
                    }
                }
                _ => {}
            }
        }
        Some("tool.call") if include_tools => {
            let call_id = event.get("toolCallId").and_then(Value::as_str);
            let name = event.get("name").and_then(Value::as_str).unwrap_or("tool");
            let input = event.get("args").cloned().unwrap_or(Value::Null);
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
        Some("tool.result") if include_tools => {
            let call_id = event
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = state
                .tool_names
                .get(call_id)
                .map(String::as_str)
                .unwrap_or("tool");
            let result = event.get("result").unwrap_or(&Value::Null);
            let output = result
                .get("output")
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string())
                })
                .unwrap_or_default();
            if !output.trim().is_empty() {
                let status = result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .map(|is_error| if is_error { "error" } else { "success" }.to_string());
                push_tool_result_entry(
                    &mut state.entries,
                    (!call_id.is_empty()).then_some(call_id),
                    name,
                    &output,
                    status,
                    HashMap::new(),
                );
            }
        }
        _ => {}
    }
}

fn text_content(content: Option<&Value>, include_system_wrappers: bool) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        return clean_text(text, include_system_wrappers);
    }
    let text = content
        .as_array()?
        .iter()
        .filter_map(|part| {
            let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
            if kind == "think" || kind == "thinking" {
                return None;
            }
            part.get("text")
                .and_then(Value::as_str)
                .and_then(|text| clean_text(text, include_system_wrappers))
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn reasoning_content(content: Option<&Value>) -> Option<String> {
    let text = content?
        .as_array()?
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("think") => part.get("think").and_then(Value::as_str),
            Some("thinking") => part
                .get("thinking")
                .or_else(|| part.get("think"))
                .and_then(Value::as_str),
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn clean_text(text: &str, include_system_wrappers: bool) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || (trimmed.starts_with("<system>CHECKPOINT ") && trimmed.ends_with("</system>"))
        || trimmed.starts_with("<system-reminder>")
    {
        return None;
    }
    if include_system_wrappers && trimmed.starts_with("<system>") && trimmed.ends_with("</system>")
    {
        let inner = trimmed
            .trim_start_matches("<system>")
            .trim_end_matches("</system>")
            .trim();
        return (!inner.is_empty()).then(|| inner.to_string());
    }
    if !include_system_wrappers && trimmed.starts_with("<system>") {
        return None;
    }
    Some(trimmed.to_string())
}
