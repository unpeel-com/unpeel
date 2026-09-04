use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "muse",
    file_backed: true,
    collect_document: None,
    collect_line,
    resume_id_from_command,
    trusted_roots,
    path_matches,
    find_by_id,
    find_best,
    title_candidate: None,
    model_from_value: Some(model_from_value),
};

fn model_from_value(value: &Value) -> Option<String> {
    let model = value.get("model_id")?.as_str()?.trim();
    // Reminder-agent configs use this placeholder; it must not shadow the
    // actual model recorded by `run.model.configured` later in the stream.
    (model != "same-as-main").then(|| model.to_string())
}

fn resume_id_from_command(command: &str) -> Option<String> {
    let words = shell_words(command);
    let resume_index = words.iter().position(|word| word == "resume")?;
    words[resume_index + 1..]
        .iter()
        .find(|word| !word.starts_with('-'))
        .cloned()
}

fn muse_sessions_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir).join("muse").join("sessions"));
    }
    Some(
        user_home_dir()?
            .join(".local")
            .join("share")
            .join("muse")
            .join("sessions"),
    )
}

fn trusted_roots() -> Vec<PathBuf> {
    muse_sessions_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"]) && path.file_name().is_some_and(|name| name == "session.jsonl")
}

fn find_by_id(_cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let root = muse_sessions_root()?;
    for year in list_dirs_sorted_desc(&root) {
        for month in list_dirs_sorted_desc(&year) {
            for day in list_dirs_sorted_desc(&month) {
                let candidate = day.join(provider_id).join("session.jsonl");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    let candidates = walk_files_with_extensions(
        &muse_sessions_root()?,
        &["jsonl"],
        PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
    )
    .into_iter()
    .filter(|path| {
        path.file_name().is_some_and(|name| name == "session.jsonl")
            && !path
                .components()
                .any(|component| component.as_os_str() == "subagent")
    })
    .filter(|path| transcript_workspace_root(path).as_deref() == Some(manifest.cwd.as_str()))
    .collect();
    best_file_for_session(candidates, manifest.session.created_at)
}

fn transcript_workspace_root(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file).take(64 * 1024);
    let mut line = String::new();
    while {
        line.clear();
        reader.read_line(&mut line).ok()? > 0
    } {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if let Some(root) = value
            .get("payload")
            .and_then(|payload| payload.get("record"))
            .and_then(|record| record.get("workspace_root"))
            .and_then(Value::as_str)
        {
            return Some(root.to_string());
        }
    }
    None
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    let Some(payload) = value.get("payload") else {
        return;
    };
    if payload.get("kind").and_then(Value::as_str) != Some("run") {
        return;
    }
    let Some(event) = payload.get("event") else {
        return;
    };
    match event.get("kind").and_then(Value::as_str) {
        Some("started") => {
            if let Some(prompt) = event.get("prompt").and_then(Value::as_str) {
                push_user_transcript_entry(&mut state.entries, prompt);
            }
        }
        Some("assistant_message_committed") => {
            if let Some(text) = event.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    push_transcript_entry(&mut state.entries, "Assistant", text.to_string());
                }
            }
        }
        Some("reasoning_committed") if include_tools => {
            if let Some(text) = event.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    push_transcript_entry(&mut state.entries, "Reasoning", text.to_string());
                }
            }
        }
        Some("assistant_tool_calls_committed") if include_tools => {
            for call in event
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let call_id = call.get("call_id").and_then(Value::as_str);
                let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
                let input = call
                    .get("args")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .unwrap_or(Value::Null);
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
        }
        Some("tool_result_batch_committed") if include_tools => {
            for result in event
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let call_id = result.get("tool_call_id").and_then(Value::as_str);
                let name = call_id
                    .and_then(|id| state.tool_names.get(id))
                    .map(String::as_str)
                    .unwrap_or("tool");
                if let Some(output) = result.get("text").and_then(Value::as_str) {
                    push_tool_result_entry(
                        &mut state.entries,
                        call_id,
                        name,
                        output,
                        None,
                        HashMap::new(),
                    );
                }
            }
        }
        _ => {}
    }
}
