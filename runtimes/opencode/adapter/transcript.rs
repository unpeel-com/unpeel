use super::*;

pub(super) const ADAPTER: TranscriptAdapter = TranscriptAdapter {
    legacy_slug: "opencode",
    file_backed: false,
    collect_document: None,
    collect_line: no_entries,
    resume_id_from_command,
    trusted_roots: no_roots,
    path_matches: no_path,
    find_by_id: not_found_by_id,
    find_best: not_found,
    title_candidate: None,
    model_from_value: None,
};

fn resume_id_from_command(command: &str) -> Option<String> {
    flag_value(&shell_words(command), &["--session"])
}

fn no_roots() -> Vec<PathBuf> {
    Vec::new()
}

fn no_path(_path: &Path) -> bool {
    false
}

fn not_found_by_id(_cwd: &str, _provider_id: &str) -> Option<PathBuf> {
    None
}

fn not_found(_manifest: &HostedSessionManifest) -> Option<PathBuf> {
    None
}

fn no_entries(_value: &Value, _include_tools: bool, _state: &mut TranscriptParseState) {}
