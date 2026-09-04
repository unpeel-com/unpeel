use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "grok",
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
    flag_value(&shell_words(command), &["--resume", "--session"])
}

fn grok_sessions_root() -> Option<PathBuf> {
    Some(user_home_dir()?.join(".grok").join("sessions"))
}

fn trusted_roots() -> Vec<PathBuf> {
    grok_sessions_root().into_iter().collect()
}

fn path_matches(path: &Path) -> bool {
    has_extension(path, &["jsonl"])
}

fn find_by_id(cwd: &str, provider_id: &str) -> Option<PathBuf> {
    let direct = grok_sessions_root()?
        .join(percent_encode_path(cwd))
        .join(provider_id)
        .join("chat_history.jsonl");
    direct.is_file().then_some(direct)
}

fn find_best(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    best_file_for_session(
        walk_files_with_extensions(
            &grok_sessions_root()?.join(percent_encode_path(&manifest.cwd)),
            &["jsonl"],
            PROVIDER_TRANSCRIPT_SEARCH_LIMIT,
        )
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == "chat_history.jsonl")
        })
        .collect(),
        manifest.session.created_at,
    )
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::new();
    for byte in path.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn collect_line(value: &Value, include_tools: bool, state: &mut TranscriptParseState) {
    match value.get("type").and_then(Value::as_str) {
        Some("user") => {
            // Grok injects skills/MCP/compaction as `type:user` with
            // `synthetic_reason`. Those are not prompts — skipping them here
            // keeps transcript auto-title from latching `<system-reminder>`
            // at SessionStart, before the real `<user_query>` exists.
            if value
                .get("synthetic_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.is_empty())
            {
                return;
            }
            if let Some(text) = text_from_content(value.get("content")) {
                push_user_transcript_entry(&mut state.entries, &text);
            }
        }
        Some("assistant") => {
            if let Some(text) = text_from_content(value.get("content")) {
                push_transcript_entry(&mut state.entries, "Assistant", text);
            }
        }
        Some("reasoning") if include_tools => {
            if let Some(text) = text_from_content(value.get("content")) {
                push_transcript_entry(&mut state.entries, "Reasoning", text);
            }
        }
        _ => {}
    }
}
