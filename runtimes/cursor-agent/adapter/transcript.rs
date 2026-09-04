use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "cursor-agent",
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
    flag_value(&shell_words(command), &["--resume", "--continue"])
}

fn cursor_projects_root() -> Option<PathBuf> {
    Some(user_home_dir()?.join(".cursor").join("projects"))
}

fn cursor_project_dir(cwd: &str) -> Option<PathBuf> {
    Some(cursor_projects_root()?.join(cwd.trim_start_matches('/').replace('/', "-")))
}

fn trusted_roots() -> Vec<PathBuf> {
    cursor_projects_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let project = cursor_project_dir(cwd)?;
    let direct = project
        .join("agent-transcripts")
        .join(provider_id)
        .join(format!("{provider_id}.jsonl"));
    if direct.is_file() {
        return Some(direct);
    }
    walk_files_with_extensions(
        &project.join("agent-transcripts"),
        &["jsonl"],
        PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
    )
    .into_iter()
    .find(|path| file_name_contains(path, provider_id))
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    best_file_for_session(
        walk_files_with_extensions(
            &cursor_project_dir(&manifest.cwd)?.join("agent-transcripts"),
            &["jsonl"],
            PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
        ),
        manifest.session.created_at,
    )
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
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
            Some("tool_use") if include_tools => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let call_id = block.get("id").and_then(Value::as_str);
                if let Some(id) = call_id {
                    state.tool_names.insert(id.to_string(), name.to_string());
                }
                let input = block.get("input").cloned().unwrap_or(Value::Null);
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
            _ => {}
        }
    }
    match role {
        "user" => push_user_transcript_entry(&mut state.entries, &text.join("\n")),
        "assistant" => push_transcript_entry(&mut state.entries, "Assistant", text.join("\n")),
        _ => {}
    }
}
