use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "cline",
    file_backed: true,
    collect_document: Some(collect_document),
    collect_line: no_line_entries,
    resume_id_from_command,
    trusted_roots,
    path_matches,
    find_by_id,
    find_best,
    title_candidate: None,
    model_from_value: Some(model_from_value),
};

fn model_from_value(value: &Value) -> Option<String> {
    value
        .get("modelInfo")?
        .get("id")?
        .as_str()
        .map(str::to_string)
}

fn resume_id_from_command(command: &str) -> Option<String> {
    flag_value(&shell_words(command), &["--id"])
}

fn cline_sessions_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLINE_SESSION_DATA_DIR").filter(|value| !value.is_empty())
    {
        return Some(resolve_provider_path(PathBuf::from(path)));
    }
    if let Some(path) = std::env::var_os("CLINE_DATA_DIR").filter(|value| !value.is_empty()) {
        return Some(resolve_provider_path(PathBuf::from(path)).join("sessions"));
    }
    let cline_home = std::env::var_os("CLINE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(resolve_provider_path)
        .unwrap_or_else(|| user_home_dir().unwrap_or_default().join(".cline"));
    Some(cline_home.join("data").join("sessions"))
}

fn trusted_roots() -> Vec<PathBuf> {
    cline_sessions_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["json"])
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".messages.json"))
}

fn find_by_id(_cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let direct = cline_sessions_root()?
        .join(provider_id)
        .join(format!("{provider_id}.messages.json"));
    direct.is_file().then_some(direct)
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    let root = cline_sessions_root()?;
    let candidates = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|dir| {
            let id = dir.file_name()?.to_str()?;
            let transcript = dir.join(format!("{id}.messages.json"));
            if !transcript.is_file() {
                return None;
            }
            let metadata_path = dir.join(format!("{id}.json"));
            let metadata: Value = serde_json::from_slice(&fs::read(metadata_path).ok()?).ok()?;
            let cwd = metadata
                .get("cwd")
                .or_else(|| metadata.get("workspace_root"))
                .and_then(Value::as_str)?;
            (cwd == manifest.cwd).then_some(transcript)
        })
        .take(PROVIDER_TRANSCRIPT_SEARCH_LIMIT)
        .collect();
    best_file_for_session(candidates, manifest.session.created_at)
}

fn no_line_entries(_value: &Value, _include_tools: bool, _state: &mut TranscriptParseState) {}

fn collect_document(value: &Value, include_tools: bool) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return entries;
    };

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let created_at = message.get("ts").and_then(Value::as_u64);
        let Some(content) = message.get("content") else {
            continue;
        };

        if let Some(text) = content.as_str() {
            match role {
                "user" => push_user_transcript_entry(&mut entries, &strip_user_input_wrapper(text)),
                "assistant" => push_structured_entry(
                    &mut entries,
                    "Assistant",
                    text.to_string(),
                    vec![TranscriptBlock {
                        id: None,
                        kind: TranscriptBlockKind::Text,
                        text: None,
                        tool_name: None,
                        status: None,
                        metadata: HashMap::new(),
                    }],
                    id.clone(),
                    None,
                    created_at,
                    4_000,
                ),
                _ => {}
            }
            continue;
        }

        let Some(blocks) = content.as_array() else {
            continue;
        };
        let visible_text = blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !visible_text.trim().is_empty() {
            match role {
                "user" => push_user_transcript_entry(
                    &mut entries,
                    &strip_user_input_wrapper(&visible_text),
                ),
                "assistant" => push_structured_entry(
                    &mut entries,
                    "Assistant",
                    visible_text,
                    vec![TranscriptBlock {
                        id: None,
                        kind: TranscriptBlockKind::Text,
                        text: None,
                        tool_name: None,
                        status: None,
                        metadata: HashMap::new(),
                    }],
                    id.clone(),
                    None,
                    created_at,
                    4_000,
                ),
                _ => {}
            }
        }

        if include_tools {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => {
                        if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                            push_structured_entry(
                                &mut entries,
                                "Reasoning",
                                thinking.to_string(),
                                vec![TranscriptBlock {
                                    id: None,
                                    kind: TranscriptBlockKind::Reasoning,
                                    text: None,
                                    tool_name: None,
                                    status: None,
                                    metadata: HashMap::new(),
                                }],
                                id.as_ref().map(|value| format!("{value}-thinking")),
                                None,
                                created_at,
                                4_000,
                            );
                        }
                    }
                    Some("tool_use") => {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let call_id = block.get("id").and_then(Value::as_str);
                        let input = block.get("input").cloned().unwrap_or(Value::Null);
                        push_tool_call_entry(
                            &mut entries,
                            call_id,
                            name,
                            summarize_tool_input(name, &input),
                            metadata_from_tool_input(
                                &input,
                                &["file_path", "path", "cmd", "command"],
                            ),
                        );
                    }
                    Some("tool_result") => {
                        let call_id = block.get("tool_use_id").and_then(Value::as_str);
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        if let Some(text) = text_from_content(block.get("content")) {
                            push_tool_result_entry(
                                &mut entries,
                                call_id,
                                name,
                                &text,
                                block
                                    .get("is_error")
                                    .and_then(Value::as_bool)
                                    .map(|is_error| {
                                        (if is_error { "failed" } else { "completed" }).to_string()
                                    }),
                                HashMap::new(),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        if include_tools && role == "assistant" {
            if let Some(metrics) = message.get("metrics").and_then(Value::as_object) {
                let mut parts = Vec::new();
                let mut metadata = HashMap::new();
                for (key, label) in [
                    ("inputTokens", "input"),
                    ("outputTokens", "output"),
                    ("cacheReadTokens", "cache read"),
                    ("cacheWriteTokens", "cache write"),
                ] {
                    if let Some(value) = metrics.get(key).and_then(Value::as_u64) {
                        parts.push(format!("{value} {label}"));
                        metadata.insert(key.to_string(), value.to_string());
                    }
                }
                if let Some(cost) = metrics.get("cost").and_then(Value::as_f64) {
                    parts.push(format!("${cost:.6}"));
                    metadata.insert("cost".to_string(), cost.to_string());
                }
                if !parts.is_empty() {
                    push_structured_entry(
                        &mut entries,
                        "Info",
                        format!("Usage: {}", parts.join(" · ")),
                        vec![TranscriptBlock {
                            id: None,
                            kind: TranscriptBlockKind::Usage,
                            text: None,
                            tool_name: None,
                            status: None,
                            metadata,
                        }],
                        id.as_ref().map(|value| format!("{value}-usage")),
                        None,
                        created_at,
                        4_000,
                    );
                }
            }
        }
    }
    entries
}

fn strip_user_input_wrapper(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("<user_input ") && trimmed.ends_with("</user_input>") {
        if let Some(open_end) = trimmed.find('>') {
            return trimmed[open_end + 1..trimmed.len() - "</user_input>".len()]
                .trim()
                .to_string();
        }
    }
    trimmed.to_string()
}
