use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "gemini",
    file_backed: true,
    collect_document: Some(collect_document),
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
    flag_value(&shell_words(command), &["--resume", "-r", "--session-id"])
}

fn gemini_tmp_root() -> Option<PathBuf> {
    Some(user_home_dir()?.join(".gemini").join("tmp"))
}

fn trusted_roots() -> Vec<PathBuf> {
    gemini_tmp_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["json", "jsonl"])
}

fn gemini_candidate_dirs(cwd: &str) -> Vec<PathBuf> {
    let Some(root) = gemini_tmp_root() else {
        return Vec::new();
    };
    let basename = Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let mut dirs = Vec::new();
    if !basename.is_empty() {
        dirs.push(root.join(basename));
    }
    if dirs.iter().all(|dir| !dir.is_dir()) {
        dirs.extend(
            fs::read_dir(root)
                .ok()
                .into_iter()
                .flat_map(|entries| entries.filter_map(Result::ok))
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    dirs
}

fn gemini_file_session_id(path: &Path) -> Option<String> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => {
            let raw = read_head_utf8(path, 64 * 1024).ok()?;
            serde_json::from_str::<Value>(&raw)
                .ok()?
                .get("sessionId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }
        Some("jsonl") => {
            let raw = read_head_utf8(path, 16 * 1024).ok()?;
            raw.lines().find_map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()?
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
        }
        _ => None,
    }
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let candidates = gemini_candidate_dirs(cwd)
        .into_iter()
        .flat_map(|dir| {
            walk_files_with_extensions(
                &dir.join("chats"),
                &["json", "jsonl"],
                PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
            )
        })
        .collect::<Vec<_>>();
    candidates.into_iter().find(|path| {
        file_name_contains(path, provider_id)
            || gemini_file_session_id(path)
                .as_deref()
                .is_some_and(|id| id == provider_id)
    })
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    best_file_for_session(
        gemini_candidate_dirs(&manifest.cwd)
            .into_iter()
            .flat_map(|dir| {
                walk_files_with_extensions(
                    &dir.join("chats"),
                    &["json", "jsonl"],
                    PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
                )
            })
            .collect(),
        manifest.session.created_at,
    )
}

fn collect_document(value: &Value, _include_tools: bool) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return entries;
    };
    for message in messages {
        match message.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(text) = text_from_content(message.get("content")) {
                    push_user_transcript_entry(&mut entries, &text);
                }
            }
            Some("gemini") | Some("assistant") => {
                if let Some(text) = text_from_content(message.get("content")) {
                    push_transcript_entry(&mut entries, "Assistant", text);
                }
            }
            Some("info") => {
                if let Some(text) = text_from_content(message.get("content")) {
                    push_transcript_entry(&mut entries, "Info", text);
                }
            }
            _ => {}
        }
    }
    entries
}

fn collect_line(value: &Value, _include_tools: bool, state: &mut TranscriptParseState) {
    match value.get("type").and_then(Value::as_str) {
        Some("user") => {
            if let Some(text) = text_from_content(value.get("content")) {
                push_user_transcript_entry(&mut state.entries, &text);
            }
        }
        Some("gemini") | Some("assistant") => {
            if let Some(text) = text_from_content(value.get("content")) {
                push_transcript_entry(&mut state.entries, "Assistant", text);
            }
        }
        Some("info") => {
            if let Some(text) = text_from_content(value.get("content")) {
                push_transcript_entry(&mut state.entries, "Info", text);
            }
        }
        _ => {}
    }
}
