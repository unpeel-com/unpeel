use crate::session_host::HostedSessionManifest;
use crate::state::current_timestamp_ms;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_TRANSCRIPT_TAIL_BYTES: u64 = 1024 * 1024;
const DEFAULT_STREAM_MAX_BYTES: u64 = 256 * 1024;
const MAX_STREAM_BYTES: u64 = 2 * 1024 * 1024;
const PROVIDER_TRANSCRIPT_SEARCH_LIMIT: usize = 1_200;

type CollectDocument = fn(&Value, bool) -> Vec<TranscriptEntry>;
type CollectLine = fn(&Value, bool, &mut TranscriptParseState);

/// Provider-owned transcript behavior compiled from one runtime package.
///
/// The registry containing these callbacks is generated from
/// `runtimes/*/runtime.toml`, so adding a transcript adapter does not require
/// changing a core enum or dispatch match.
pub struct TranscriptAdapter {
    legacy_slug: &'static str,
    file_backed: bool,
    collect_document: Option<CollectDocument>,
    collect_line: CollectLine,
    resume_id_from_command: fn(&str) -> Option<String>,
    trusted_roots: fn() -> Vec<PathBuf>,
    path_matches: fn(&Path) -> bool,
    find_by_id: fn(&str, &str) -> Option<PathBuf>,
    find_best: fn(&HostedSessionManifest) -> Option<PathBuf>,
    title_candidate: Option<fn(&str) -> Option<String>>,
    model_from_value: Option<fn(&Value) -> Option<String>>,
}

#[derive(Clone, Copy)]
pub struct TranscriptProvider(&'static TranscriptAdapter);

impl std::fmt::Debug for TranscriptProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("TranscriptProvider")
            .field(&self.as_str())
            .finish()
    }
}

impl PartialEq for TranscriptProvider {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for TranscriptProvider {}

impl Serialize for TranscriptProvider {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

include!(concat!(
    env!("OUT_DIR"),
    "/transcript_adapters_generated.rs"
));

impl TranscriptProvider {
    pub fn as_str(self) -> &'static str {
        self.0.legacy_slug
    }

    fn not_yet_file_backed(self) -> bool {
        !self.0.file_backed
    }

    fn adapter(self) -> &'static TranscriptAdapter {
        self.0
    }

    pub(crate) fn for_legacy_slug(slug: &str) -> Option<Self> {
        TRANSCRIPT_ADAPTERS
            .iter()
            .copied()
            .find(|adapter| adapter.legacy_slug.eq_ignore_ascii_case(slug.trim()))
            .map(Self)
    }
}

#[derive(Default)]
struct TranscriptParseState {
    entries: Vec<TranscriptEntry>,
    tool_names: HashMap<String, String>,
    tool_inputs: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderTranscript {
    pub provider: TranscriptProvider,
    pub path: PathBuf,
    pub provider_session_id: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptBlockKind {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
    Permission,
    Info,
    FileChange,
    Diff,
    PlanUpdate,
    Usage,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: TranscriptBlockKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    pub role: &'static str,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<TranscriptBlock>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSnapshot {
    pub session_id: String,
    pub provider: String,
    pub source: String,
    pub provider_session_id: Option<String>,
    pub path: String,
    pub start_offset: u64,
    pub entries: Vec<TranscriptEntry>,
    pub next_offset: u64,
    pub updated_at: u64,
    /// Last model name seen in the read window (e.g. Claude's
    /// `message.model`, Codex's turn-context `model`). Best-effort — absent
    /// for providers that never record one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptHistoryPage {
    pub session_id: String,
    pub provider: String,
    pub source: String,
    pub provider_session_id: Option<String>,
    pub path: String,
    pub offset: u64,
    pub next_offset: u64,
    pub truncated: bool,
    pub entries: Vec<TranscriptEntry>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptStreamChunk {
    pub session_id: String,
    pub provider: String,
    pub source: String,
    pub provider_session_id: Option<String>,
    pub path: String,
    pub offset: u64,
    pub next_offset: u64,
    pub partial: String,
    pub truncated: bool,
    pub entries: Vec<TranscriptEntry>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonlStreamRead {
    pub offset: u64,
    pub next_offset: u64,
    pub partial: String,
    pub truncated: bool,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonlWindowRead {
    pub offset: u64,
    pub next_offset: u64,
    pub truncated: bool,
    pub lines: Vec<(u64, String)>,
}

pub fn provider_label_for_command(command: &str) -> String {
    if let Some(provider) = transcript_provider_for_command(command) {
        return provider.as_str().to_string();
    }
    let Some(head) = shell_words(command).first().cloned() else {
        return "shell".to_string();
    };
    Path::new(&head)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&head)
        .to_ascii_lowercase()
}

pub fn transcript_status_hint(manifest: &HostedSessionManifest) -> &'static str {
    let Some(provider) = transcript_provider_for_command(&manifest.session.command) else {
        return "none";
    };
    if provider.not_yet_file_backed() {
        return "planned";
    }
    if resolve_provider_transcript(manifest).is_ok() {
        "available"
    } else {
        "supported"
    }
}

pub fn transcript_provider_for_command(command: &str) -> Option<TranscriptProvider> {
    let words = shell_words(command);
    let head = words.first()?.as_str();
    let head = Path::new(head)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(head)
        .to_ascii_lowercase();
    let runtime = crate::runtime_catalog::builtin_runtime_catalog()
        .by_command_alias_for_current_platform(&head)?;
    TranscriptProvider::for_legacy_slug(&runtime.legacy_slug)
}

pub fn resume_id_from_command(provider: TranscriptProvider, command: &str) -> Option<String> {
    (provider.adapter().resume_id_from_command)(command)
}

pub fn resolve_provider_transcript(
    manifest: &HostedSessionManifest,
) -> Result<ProviderTranscript, String> {
    let provider = transcript_provider_for_command(&manifest.session.command).ok_or_else(|| {
        format!(
            "Session '{}' is not a supported transcript-backed provider session.",
            manifest.session.id
        )
    })?;

    if provider.not_yet_file_backed() {
        return Err(format!(
            "{} transcripts are stored outside JSONL and need a provider-specific adapter.",
            provider.as_str()
        ));
    }

    if let Some(path) = crate::session_ops::provider_session_marker(&manifest.session.id)
        .1
        .as_deref()
        .or(manifest.provider_transcript_path.as_deref())
        .map(PathBuf::from)
        .filter(|path| trusted_provider_transcript_path(provider, path))
        .filter(|path| path.is_file())
    {
        return Ok(ProviderTranscript {
            provider,
            path,
            provider_session_id: manifest.provider_session_id.clone(),
            source: "manifest",
        });
    }

    if let Some(provider_id) = manifest.provider_session_id.as_deref() {
        if let Some(path) = find_transcript_by_provider_id(provider, &manifest.cwd, provider_id) {
            return Ok(ProviderTranscript {
                provider,
                path,
                provider_session_id: Some(provider_id.to_string()),
                source: "provider_session_id",
            });
        }
    }

    if let Some(resume_id) = resume_id_from_command(provider, &manifest.session.command) {
        if let Some(path) = find_transcript_by_provider_id(provider, &manifest.cwd, &resume_id) {
            return Ok(ProviderTranscript {
                provider,
                path,
                provider_session_id: Some(resume_id),
                source: "resume_arg",
            });
        }
    }

    let discovered = (provider.adapter().find_best)(manifest)
        .filter(|path| trusted_provider_transcript_path(provider, path));
    discovered
        .map(|path| ProviderTranscript {
            provider,
            path,
            provider_session_id: None,
            source: "cwd_match",
        })
        .ok_or_else(|| {
            format!(
                "No {} transcript found for session '{}' in cwd {}.",
                provider.as_str(),
                manifest.session.id,
                manifest.cwd
            )
        })
}

pub fn read_transcript_snapshot(
    manifest: &HostedSessionManifest,
    max_entries: usize,
    include_tools: bool,
    tail_bytes: Option<u64>,
) -> Result<TranscriptSnapshot, String> {
    let transcript = resolve_provider_transcript(manifest)?;
    let (entries, start_offset, next_offset, model) =
        match transcript.path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => {
                let raw = read_head_utf8(&transcript.path, u64::MAX)?;
                let mut entries =
                    collect_transcript_entries(transcript.provider, &raw, include_tools);
                if entries.len() > max_entries {
                    entries = entries[entries.len().saturating_sub(max_entries)..].to_vec();
                }
                let next_offset = fs::metadata(&transcript.path)
                    .map_err(|e| format!("Failed to stat {}: {e}", transcript.path.display()))?
                    .len();
                let model = serde_json::from_str::<Value>(&raw)
                    .ok()
                    .as_ref()
                    .and_then(|value| find_model_string(transcript.provider, value));
                (entries, 0, next_offset, model)
            }
            _ => {
                let read = read_jsonl_lines_before(
                    &transcript.path,
                    None,
                    tail_bytes.unwrap_or(DEFAULT_TRANSCRIPT_TAIL_BYTES),
                )?;
                let (entries, start_offset) = collect_transcript_entries_from_window(
                    transcript.provider,
                    &read.lines,
                    include_tools,
                    max_entries,
                );
                let model = detect_model_in_lines(
                    transcript.provider,
                    read.lines.iter().map(|(_, line)| line.as_str()),
                );
                (entries, start_offset, read.next_offset, model)
            }
        };

    Ok(TranscriptSnapshot {
        session_id: manifest.session.id.clone(),
        provider: transcript.provider.as_str().to_string(),
        source: transcript.source.to_string(),
        provider_session_id: transcript.provider_session_id,
        path: transcript.path.display().to_string(),
        start_offset,
        entries,
        next_offset,
        updated_at: current_timestamp_ms(),
        model,
    })
}

/// Last model name recorded in a window of provider JSONL lines. Providers
/// store it in different places (Claude: `message.model` on assistant lines,
/// Codex: `payload.model` on turn-context lines), so this is a generic
/// bounded-depth search for a non-empty string under a `model` key.
fn detect_model_in_lines<'a>(
    provider: TranscriptProvider,
    lines: impl Iterator<Item = &'a str>,
) -> Option<String> {
    let mut model = None;
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(found) = find_model_string(provider, &value) {
            model = Some(found);
        }
    }
    model
}

/// Depth-first search for the first non-empty string value under a `model`
/// key, bounded so hostile/degenerate JSON can't recurse deeply. Skips
/// obvious non-model values (paths, sentences).
fn find_model_string(provider: TranscriptProvider, value: &Value) -> Option<String> {
    fn valid_model(model: String) -> Option<String> {
        let model = model.trim();
        (!model.is_empty() && model.len() <= 64 && !model.contains(char::is_whitespace))
            .then(|| model.to_string())
    }

    fn walk(provider: TranscriptProvider, value: &Value, depth: u8) -> Option<String> {
        if depth == 0 {
            return None;
        }
        match value {
            Value::Object(obj) => {
                let direct_model = provider
                    .adapter()
                    .model_from_value
                    .and_then(|find| find(value))
                    .or_else(|| obj.get("model").and_then(Value::as_str).map(str::to_string));
                if let Some(model) = direct_model.and_then(valid_model) {
                    return Some(model);
                }
                obj.values()
                    .find_map(|child| walk(provider, child, depth - 1))
            }
            Value::Array(items) => items
                .iter()
                .find_map(|child| walk(provider, child, depth - 1)),
            _ => None,
        }
    }
    walk(provider, value, 6)
}

pub fn read_transcript_history(
    manifest: &HostedSessionManifest,
    before_offset: Option<u64>,
    max_entries: usize,
    include_tools: bool,
    max_bytes: Option<u64>,
) -> Result<TranscriptHistoryPage, String> {
    let transcript = resolve_provider_transcript(manifest)?;
    if transcript.path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return Err(format!(
            "{} transcript history paging requires a JSONL source, got {}.",
            transcript.provider.as_str(),
            transcript.path.display()
        ));
    }
    let read = read_jsonl_lines_before(
        &transcript.path,
        before_offset,
        max_bytes.unwrap_or(DEFAULT_TRANSCRIPT_TAIL_BYTES),
    )?;
    let (entries, offset) = collect_transcript_entries_from_window(
        transcript.provider,
        &read.lines,
        include_tools,
        max_entries,
    );

    Ok(TranscriptHistoryPage {
        session_id: manifest.session.id.clone(),
        provider: transcript.provider.as_str().to_string(),
        source: transcript.source.to_string(),
        provider_session_id: transcript.provider_session_id,
        path: transcript.path.display().to_string(),
        offset,
        next_offset: read.next_offset,
        truncated: offset > 0 || read.truncated,
        entries,
        updated_at: current_timestamp_ms(),
    })
}

pub fn read_transcript_stream(
    manifest: &HostedSessionManifest,
    offset: u64,
    partial: &str,
    include_tools: bool,
    max_bytes: Option<u64>,
) -> Result<TranscriptStreamChunk, String> {
    let transcript = resolve_provider_transcript(manifest)?;
    if transcript.path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return Err(format!(
            "{} transcript streaming requires a JSONL source, got {}.",
            transcript.provider.as_str(),
            transcript.path.display()
        ));
    }
    let read = read_jsonl_lines_since(
        &transcript.path,
        offset,
        partial,
        max_bytes.unwrap_or(DEFAULT_STREAM_MAX_BYTES),
    )?;
    let raw = read.lines.join("\n");
    let entries = collect_transcript_entries(transcript.provider, &raw, include_tools);
    Ok(TranscriptStreamChunk {
        session_id: manifest.session.id.clone(),
        provider: transcript.provider.as_str().to_string(),
        source: transcript.source.to_string(),
        provider_session_id: transcript.provider_session_id,
        path: transcript.path.display().to_string(),
        offset: read.offset,
        next_offset: read.next_offset,
        partial: read.partial,
        truncated: read.truncated,
        entries,
        updated_at: current_timestamp_ms(),
    })
}

pub fn read_jsonl_lines_since(
    path: &Path,
    requested_offset: u64,
    previous_partial: &str,
    max_bytes: u64,
) -> Result<JsonlStreamRead, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?
        .len();
    let max_bytes = max_bytes.clamp(1, MAX_STREAM_BYTES);
    let mut truncated = len < requested_offset;
    let mut offset = if truncated { 0 } else { requested_offset };
    let mut partial = if truncated {
        String::new()
    } else {
        previous_partial.to_string()
    };

    if len.saturating_sub(offset) > max_bytes {
        offset = len.saturating_sub(max_bytes);
        partial.clear();
        truncated = true;
    }

    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("Failed to seek {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    file.take(max_bytes)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let next_offset = offset + buf.len() as u64;
    let chunk = String::from_utf8_lossy(&buf);
    partial.push_str(&chunk);
    let mut lines: Vec<String> = partial.split('\n').map(ToString::to_string).collect();
    partial = lines.pop().unwrap_or_default();
    if offset > 0 && requested_offset != offset && !lines.is_empty() {
        lines.remove(0);
    }
    let lines = lines
        .into_iter()
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.trim().is_empty())
        .collect();

    Ok(JsonlStreamRead {
        offset,
        next_offset,
        partial,
        truncated,
        lines,
    })
}

pub fn read_jsonl_lines_before(
    path: &Path,
    before_offset: Option<u64>,
    max_bytes: u64,
) -> Result<JsonlWindowRead, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|e| format!("Failed to stat {}: {e}", path.display()))?
        .len();
    let max_bytes = max_bytes.clamp(1, MAX_STREAM_BYTES);
    let end = before_offset.unwrap_or(len).min(len);
    let start = end.saturating_sub(max_bytes);

    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Failed to seek {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    file.take(end.saturating_sub(start))
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    let mut offset = start;

    if start > 0 {
        if let Some(index) = text.find('\n') {
            let next = index + 1;
            offset += next as u64;
            text = text[next..].to_string();
        } else {
            return Ok(JsonlWindowRead {
                offset: end,
                next_offset: end,
                truncated: end > 0,
                lines: Vec::new(),
            });
        }
    }

    let mut cursor = offset;
    let mut lines = Vec::new();
    for segment in text.split_inclusive('\n') {
        let line_offset = cursor;
        cursor += segment.len() as u64;
        let line = segment.trim_end_matches(['\r', '\n']).to_string();
        if !line.trim().is_empty() {
            lines.push((line_offset, line));
        }
    }

    if !text.ends_with('\n') {
        let consumed: u64 = text
            .split_inclusive('\n')
            .map(|segment| segment.len() as u64)
            .sum();
        if consumed < text.len() as u64 {
            let line = text[consumed as usize..]
                .trim_end_matches(['\r', '\n'])
                .to_string();
            if !line.trim().is_empty() {
                lines.push((offset + consumed, line));
            }
        }
    }

    Ok(JsonlWindowRead {
        offset,
        next_offset: end,
        truncated: offset > 0,
        lines,
    })
}

pub fn collect_transcript_entries(
    provider: TranscriptProvider,
    raw: &str,
    include_tools: bool,
) -> Vec<TranscriptEntry> {
    let adapter = provider.adapter();
    if let Some(collect_document) = adapter.collect_document {
        if let Ok(value) = serde_json::from_str::<Value>(raw) {
            return collect_document(&value, include_tools);
        }
    }

    let mut state = TranscriptParseState::default();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        (adapter.collect_line)(&value, include_tools, &mut state);
    }
    state.entries
}

fn collect_transcript_entries_from_window(
    provider: TranscriptProvider,
    lines: &[(u64, String)],
    include_tools: bool,
    max_entries: usize,
) -> (Vec<TranscriptEntry>, u64) {
    let adapter = provider.adapter();
    let mut state = TranscriptParseState::default();
    let mut entry_offsets = Vec::new();

    for (offset, line) in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let previous_len = state.entries.len();
        (adapter.collect_line)(&value, include_tools, &mut state);
        entry_offsets.extend(std::iter::repeat_n(
            *offset,
            state.entries.len() - previous_len,
        ));
    }

    if state.entries.len() > max_entries {
        let start = state.entries.len().saturating_sub(max_entries);
        let offset = entry_offsets
            .get(start)
            .copied()
            .unwrap_or_else(|| lines.first().map(|(offset, _)| *offset).unwrap_or(0));
        (state.entries[start..].to_vec(), offset)
    } else {
        let offset = entry_offsets
            .first()
            .copied()
            .unwrap_or_else(|| lines.first().map(|(offset, _)| *offset).unwrap_or(0));
        (state.entries, offset)
    }
}

/// Bytes of JSONL tail scanned when rendering a full Markdown transcript. Larger
/// than the default snapshot tail so "whole conversation" captures more history.
const TRANSCRIPT_MARKDOWN_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Read the app-wide [`TranscriptSettings`](crate::state::TranscriptSettings)
/// from `app-state.json`, falling back to the shipped defaults when the file is
/// absent or malformed. Shared by the Markdown CLI and the Sessions MCP
/// `read_transcript` tool so both honor the same options.
pub fn load_transcript_settings() -> crate::state::TranscriptSettings {
    std::fs::read(crate::app_paths::app_state_path())
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("transcript_settings")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
        })
        .unwrap_or_default()
}

/// Cap on the transcript head read when titling a session from its provider
/// conversation. Titles come from the conversation's opening records, so a
/// bounded head window is enough even for huge JSONL files.
const TITLE_HEAD_BYTES: u64 = 256 * 1024;

/// Title an untitled session from the provider conversation it is linked to
/// — Claude's `summary` record when present, otherwise the conversation's
/// first real user prompt. Called (via `unpeel-host __auto_title__` from the
/// app, in-process from the TUI hook listener) when a hook capture *changes*
/// a session's provider metadata: the user resumed or switched conversations
/// inside the tool, and the line they typed to get there was a slash command,
/// which `normalize_prompt_title` deliberately never titles from. No-ops once
/// titling is settled, when the transcript cannot be resolved by id or
/// captured path (the cwd_match fallback could belong to a *different*
/// conversation, so it must never title), or when nothing in the head window
/// normalizes to a title. Returns true when titling is settled.
pub fn auto_title_session_from_transcript(session_id: &str) -> bool {
    let Some(manifest) = crate::session_host::load_manifest(session_id) else {
        return false;
    };
    let session = &manifest.session;
    // Same settled checks as apply_manifest_auto_title, done up front so a
    // titled session never pays for a transcript read.
    if session.command.is_empty()
        || session.custom_title
        || !session.command.starts_with(session.label.as_str())
    {
        return false;
    }
    let Ok(transcript) = resolve_provider_transcript(&manifest) else {
        return false;
    };
    if transcript.source == "cwd_match" {
        return false;
    }
    let raw = match transcript.path.extension().and_then(|ext| ext.to_str()) {
        // Whole-document formats (gemini/cline) only parse complete.
        Some("json") => read_head_utf8(&transcript.path, u64::MAX),
        _ => read_head_utf8(&transcript.path, TITLE_HEAD_BYTES),
    };
    let Ok(raw) = raw else {
        return false;
    };
    let Some(candidate) = transcript_title_candidate(transcript.provider, &raw) else {
        return false;
    };
    crate::session_host::apply_manifest_auto_title(session_id, &candidate)
}

/// Best title line in the head of a provider conversation. Claude `summary`
/// records win (they are the conversation titles Claude's own /resume picker
/// shows); otherwise the first user prompt that survives sanitizing and
/// `normalize_prompt_title` (slash commands, image-only prompts, and
/// compact-continuation preambles are skipped, falling through to the next
/// user entry). A truncated trailing JSONL line simply fails to parse and is
/// ignored.
fn transcript_title_candidate(provider: TranscriptProvider, raw: &str) -> Option<String> {
    if let Some(candidate) = provider
        .adapter()
        .title_candidate
        .and_then(|candidate| candidate(raw))
    {
        return Some(candidate);
    }
    collect_transcript_entries(provider, raw, false)
        .iter()
        .filter(|entry| entry.role == "User")
        .filter(|entry| {
            !entry
                .text
                .trim_start()
                .starts_with("This session is being continued")
        })
        .find_map(|entry| {
            let line = entry.text.lines().find(|line| !line.trim().is_empty())?;
            crate::session_host::normalize_prompt_title(line)
        })
}

/// Resolve the provider transcript and render it as Markdown according to
/// `opts`. Collects with tools enabled whenever any structured content
/// (reasoning, tools, file changes, plan updates) is requested, then filters
/// per entry so each toggle is honored independently.
pub fn read_transcript_markdown(
    manifest: &HostedSessionManifest,
    opts: &crate::state::TranscriptSettings,
) -> Result<String, String> {
    let collect_tools = opts.include_tools
        || opts.include_reasoning
        || opts.include_file_changes
        || opts.include_plan_updates;
    let max_entries = if opts.max_entries == 0 {
        usize::MAX
    } else {
        opts.max_entries
    };
    let snapshot = read_transcript_snapshot(
        manifest,
        max_entries,
        collect_tools,
        Some(TRANSCRIPT_MARKDOWN_TAIL_BYTES),
    )?;
    let body = format_transcript_markdown(&snapshot, opts);
    if !opts.include_session_info {
        return Ok(body);
    }
    Ok(format!(
        "{}\n{}",
        session_info_header(&manifest.session, &snapshot),
        body
    ))
}

/// Resolve one Unpeel session and render its provider conversation with the
/// shared app-wide transcript settings. This is the common entry point for
/// frontend copy actions and the `__transcript__ markdown` CLI: `entries`
/// overrides the configured range (`0` means the whole conversation), while
/// `include_tools` is the CLI's opt-in override.
pub fn read_session_transcript_markdown(
    session_id: &str,
    entries: Option<usize>,
    include_tools: bool,
) -> Result<String, String> {
    let manifest = load_safe_manifest(session_id)
        .ok_or_else(|| format!("Unknown or invalid session id '{session_id}'."))?;
    let mut settings = load_transcript_settings();
    if let Some(entries) = entries {
        settings.max_entries = entries;
    }
    if include_tools {
        settings.include_tools = true;
    }
    read_transcript_markdown(&manifest, &settings)
}

/// The session-info header the Settings "Session info" toggle controls:
/// title, the Unpeel session id (a valid target for Unpeel Sessions MCP
/// tools, so a pasted transcript is actionable by other agents), CLI, model
/// when the provider records one, and the launch command.
fn session_info_header(
    session: &crate::state::SessionInfo,
    snapshot: &TranscriptSnapshot,
) -> String {
    let mut out = String::new();
    let title = session.label.trim();
    out.push_str(&format!(
        "# {}\n\n",
        if title.is_empty() {
            "Session transcript"
        } else {
            title
        }
    ));
    out.push_str(&format!(
        "- Unpeel session ID: `{}` (target id for the Unpeel MCP sessions tool)\n",
        session.id
    ));
    out.push_str(&format!("- CLI: {}\n", snapshot.provider));
    if let Some(model) = &snapshot.model {
        out.push_str(&format!("- Model: {model}\n"));
    }
    let command = session.command.trim();
    if !command.is_empty() {
        out.push_str(&format!("- Command: `{command}`\n"));
    }
    out
}

/// Render already-collected transcript entries as Markdown, filtering each entry
/// by the content-type toggles in `opts`.
pub fn format_transcript_markdown(
    snapshot: &TranscriptSnapshot,
    opts: &crate::state::TranscriptSettings,
) -> String {
    let mut out = String::new();
    for entry in &snapshot.entries {
        if !transcript_entry_included(entry, opts) {
            continue;
        }
        append_entry_markdown(&mut out, entry);
    }
    out.trim_end().to_string()
}

fn primary_block_kind(entry: &TranscriptEntry) -> Option<TranscriptBlockKind> {
    entry.blocks.first().map(|block| block.kind)
}

fn transcript_entry_included(
    entry: &TranscriptEntry,
    opts: &crate::state::TranscriptSettings,
) -> bool {
    match primary_block_kind(entry) {
        Some(TranscriptBlockKind::Reasoning) => opts.include_reasoning,
        Some(TranscriptBlockKind::PlanUpdate) => opts.include_plan_updates,
        Some(TranscriptBlockKind::FileChange) | Some(TranscriptBlockKind::Diff) => {
            opts.include_file_changes
        }
        Some(TranscriptBlockKind::ToolCall) | Some(TranscriptBlockKind::ToolResult) => {
            opts.include_tools
        }
        _ => match entry.role {
            "User" => opts.include_user,
            "Reasoning" => opts.include_reasoning,
            // Generic informational entries ride with the assistant toggle.
            _ => opts.include_assistant,
        },
    }
}

fn append_entry_markdown(out: &mut String, entry: &TranscriptEntry) {
    let kind = primary_block_kind(entry);
    let block_text = entry
        .blocks
        .first()
        .and_then(|block| block.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let tool_name = entry
        .blocks
        .first()
        .and_then(|block| block.tool_name.as_deref())
        .unwrap_or("tool");

    match kind {
        Some(TranscriptBlockKind::Reasoning) => {
            out.push_str("### Reasoning\n\n");
            for line in entry.text.trim().lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        Some(TranscriptBlockKind::PlanUpdate) => {
            out.push_str("### Plan update\n\n");
            let body = block_text.unwrap_or_else(|| entry.text.trim());
            for line in body.lines() {
                out.push_str("- ");
                out.push_str(line.trim());
                out.push('\n');
            }
            out.push('\n');
        }
        Some(TranscriptBlockKind::Diff) => {
            out.push_str(&format!("### {}\n\n", entry.text.trim()));
            out.push_str("```diff\n");
            if let Some(text) = block_text {
                out.push_str(text);
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
        Some(TranscriptBlockKind::FileChange) => {
            out.push_str(&format!("### {}\n\n", entry.text.trim()));
        }
        Some(TranscriptBlockKind::ToolCall) | Some(TranscriptBlockKind::ToolResult) => {
            let label = if matches!(kind, Some(TranscriptBlockKind::ToolResult)) {
                "Tool result"
            } else {
                "Tool"
            };
            out.push_str(&format!("### {label}: {tool_name}\n\n"));
            if let Some(text) = block_text {
                out.push_str("```\n");
                out.push_str(text);
                out.push_str("\n```\n\n");
            }
        }
        _ => {
            let heading = match entry.role {
                "User" => "## User".to_string(),
                "Assistant" => "## Assistant".to_string(),
                "Info" => "### Info".to_string(),
                other => format!("## {other}"),
            };
            out.push_str(&heading);
            out.push_str("\n\n");
            out.push_str(entry.text.trim());
            out.push_str("\n\n");
        }
    }
}

pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mode = args.first().map(String::as_str).unwrap_or("snapshot");
    let session_id = args.get(1).ok_or(
        "usage: unpeel-host __transcript__ snapshot|stream|history|markdown <session-id> [options]",
    )?;
    let include_tools = args.iter().any(|arg| arg == "--include-tools");
    let entries = flag_usize(args, "--entries").unwrap_or(50).clamp(1, 500);
    let max_bytes = flag_u64(args, "--max-bytes");

    if mode == "markdown" {
        // Settings from app-state.json are the defaults; optional flags
        // override for ad-hoc CLI use. Frontend copy actions call the same
        // shared wrapper directly (or, from Swift, through this CLI).
        let markdown = read_session_transcript_markdown(
            session_id,
            flag_usize(args, "--entries"),
            include_tools,
        )?;
        println!("{markdown}");
        return Ok(());
    }

    let manifest = load_safe_manifest(session_id)
        .ok_or_else(|| format!("Unknown or invalid session id '{session_id}'."))?;

    let value =
        match mode {
            "snapshot" => serde_json::to_value(read_transcript_snapshot(
                &manifest,
                entries,
                include_tools,
                max_bytes,
            )?)
            .map_err(|e| format!("Failed to encode transcript snapshot: {e}"))?,
            "history" => {
                let before_offset = flag_u64(args, "--before-offset");
                serde_json::to_value(read_transcript_history(
                    &manifest,
                    before_offset,
                    entries,
                    include_tools,
                    max_bytes,
                )?)
                .map_err(|e| format!("Failed to encode transcript history page: {e}"))?
            }
            "stream" => {
                let offset = flag_u64(args, "--offset").unwrap_or(0);
                let partial = flag_string(args, "--partial").unwrap_or_default();
                serde_json::to_value(read_transcript_stream(
                    &manifest,
                    offset,
                    &partial,
                    include_tools,
                    max_bytes,
                )?)
                .map_err(|e| format!("Failed to encode transcript stream chunk: {e}"))?
            }
            _ => return Err(
                "usage: unpeel-host __transcript__ snapshot|stream|history|markdown <session-id> [options]"
                    .to_string(),
            ),
        };
    let body = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("Failed to serialize transcript response: {e}"))?;
    println!("{body}");
    Ok(())
}

fn load_safe_manifest(session_id: &str) -> Option<HostedSessionManifest> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains("..")
        || session_id.contains('\\')
    {
        return None;
    }
    crate::session_host::load_manifest(session_id)
}

fn text_from_content(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .filter_map(|block| {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                return Some(text);
            }
            if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                return Some(thinking);
            }
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(kind, "text" | "input_text" | "output_text") {
                block.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn push_user_transcript_entry(entries: &mut Vec<TranscriptEntry>, text: &str) {
    if let Some(text) = sanitize_transcript_user_text(text) {
        push_transcript_entry(entries, "User", text);
    }
}

fn sanitize_transcript_user_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if let Some(query) = inner_xml_tag(trimmed, "user_query") {
        return sanitize_transcript_user_text(query);
    }
    let command_wrapper = (trimmed.contains("<command-name>")
        && trimmed.contains("</command-name>"))
        || (trimmed.contains("<command-message>") && trimmed.contains("</command-message>"))
        || (trimmed.contains("<command-args>") && trimmed.contains("</command-args>"));
    if trimmed.is_empty()
        || trimmed.starts_with("<turn_aborted>")
        || trimmed.starts_with("<subagent_notification>")
        || trimmed.starts_with("<local-command-")
        || trimmed.starts_with("# AGENTS.md instructions for ")
        || trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("<user_info>")
        || trimmed.starts_with("<user_query>")
        || command_wrapper
    {
        return None;
    }
    Some(strip_origin_tag_lines(trimmed))
}

fn inner_xml_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let inner = text[start..end].trim();
    (!inner.is_empty()).then_some(inner)
}

fn strip_origin_tag_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while lines.last().is_some_and(|line| {
        let line = line.trim();
        line.starts_with("[sent from ") && line.contains(" session_id=\"") && line.ends_with(']')
    }) {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

fn push_transcript_entry(entries: &mut Vec<TranscriptEntry>, role: &'static str, text: String) {
    let kind = match role {
        "Reasoning" => TranscriptBlockKind::Reasoning,
        "Info" => TranscriptBlockKind::Info,
        "Tool" => TranscriptBlockKind::ToolCall,
        _ => TranscriptBlockKind::Text,
    };
    push_structured_entry(
        entries,
        role,
        text,
        vec![TranscriptBlock {
            id: None,
            kind,
            text: None,
            tool_name: None,
            status: None,
            metadata: HashMap::new(),
        }],
        None,
        None,
        None,
        4_000,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_structured_entry(
    entries: &mut Vec<TranscriptEntry>,
    role: &'static str,
    text: String,
    mut blocks: Vec<TranscriptBlock>,
    id: Option<String>,
    sequence: Option<u64>,
    created_at: Option<u64>,
    max_chars: usize,
) {
    let text = truncate_text(text.trim(), max_chars);
    if text.is_empty() {
        return;
    }
    for block in &mut blocks {
        if block.text.is_none() {
            block.text = Some(text.clone());
        } else if let Some(block_text) = block.text.as_deref() {
            block.text = Some(truncate_text(block_text, max_chars));
        }
    }
    if entries
        .last()
        .is_some_and(|entry| entry.role == role && entry.text == text)
    {
        return;
    }
    entries.push(TranscriptEntry {
        id,
        sequence,
        created_at,
        role,
        text,
        blocks,
    });
}

fn push_tool_call_entry(
    entries: &mut Vec<TranscriptEntry>,
    call_id: Option<&str>,
    name: &str,
    summary: String,
    metadata: HashMap<String, String>,
) {
    let text = if summary.trim().is_empty() {
        name.to_string()
    } else {
        format!("{name}: {}", summary.trim())
    };
    push_structured_entry(
        entries,
        "Tool",
        text,
        vec![TranscriptBlock {
            id: call_id.map(str::to_string),
            kind: TranscriptBlockKind::ToolCall,
            text: Some(summary),
            tool_name: Some(name.to_string()),
            status: None,
            metadata,
        }],
        call_id.map(|id| format!("tool-call-{id}")),
        None,
        None,
        4_000,
    );
}

fn push_tool_result_entry(
    entries: &mut Vec<TranscriptEntry>,
    call_id: Option<&str>,
    name: &str,
    output: &str,
    status: Option<String>,
    metadata: HashMap<String, String>,
) {
    let text = format!("{name} result: {}", truncate_text(output, 800));
    push_structured_entry(
        entries,
        "Tool",
        text,
        vec![TranscriptBlock {
            id: call_id.map(str::to_string),
            kind: TranscriptBlockKind::ToolResult,
            text: Some(truncate_text(output, 2_000)),
            tool_name: Some(name.to_string()),
            status,
            metadata,
        }],
        call_id.map(|id| format!("tool-result-{id}")),
        None,
        None,
        4_000,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_file_change_entry(
    entries: &mut Vec<TranscriptEntry>,
    id: Option<String>,
    tool_name: Option<String>,
    path: Option<String>,
    operation: &str,
    diff: Option<String>,
    status: Option<String>,
    mut metadata: HashMap<String, String>,
) {
    if let Some(path) = path.as_deref() {
        metadata.insert("path".to_string(), path.to_string());
    }
    metadata.insert("operation".to_string(), operation.to_string());
    let kind = if diff.is_some() {
        TranscriptBlockKind::Diff
    } else {
        TranscriptBlockKind::FileChange
    };
    let summary_path = path.as_deref().unwrap_or("file");
    let text = match operation {
        "write" => format!("Wrote {summary_path}"),
        "move" => format!("Moved {summary_path}"),
        "delete" => format!("Deleted {summary_path}"),
        "patch" => format!("Patched {summary_path}"),
        "edit" | "replace" => format!("Edited {summary_path}"),
        other => format!("{other} {summary_path}"),
    };
    let block_text = diff.unwrap_or_else(|| text.clone());
    let mut max_chars = 6_000;
    if matches!(kind, TranscriptBlockKind::FileChange) {
        max_chars = 1_000;
    }
    push_structured_entry(
        entries,
        "Tool",
        text,
        vec![TranscriptBlock {
            id: id.clone(),
            kind,
            text: Some(block_text),
            tool_name,
            status,
            metadata,
        }],
        id.map(|id| format!("file-change-{id}")),
        None,
        None,
        max_chars,
    );
}

fn summarize_tool_input(name: &str, input: &Value) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str);
    let summary = match name {
        "Bash" | "bash" | "exec_command" => field("command").or_else(|| field("cmd")),
        "Read" | "Edit" | "Write" => field("file_path").or_else(|| field("path")),
        "Glob" => field("pattern"),
        "Grep" => field("pattern").or_else(|| field("query")),
        "Task" | "spawn_agent" => field("description").or_else(|| field("message")),
        "WebSearch" => field("query"),
        _ => None,
    };
    if let Some(summary) = summary {
        return truncate_text(summary, 160);
    }
    if input.is_null() {
        return String::new();
    }
    serde_json::to_string(input)
        .map(|value| truncate_text(&value, 160))
        .unwrap_or_default()
}

fn string_field<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn metadata_from_tool_input(input: &Value, keys: &[&str]) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    for key in keys {
        if let Some(value) = input.get(*key) {
            if let Some(text) = value.as_str() {
                metadata.insert((*key).to_string(), truncate_text(text, 400));
            } else if value.is_boolean() || value.is_number() {
                metadata.insert((*key).to_string(), value.to_string());
            }
        }
    }
    metadata
}

fn diff_line_counts(diff: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

fn synthetic_edit_diff(path: &str, old: &str, new: &str) -> String {
    let mut diff = format!("--- a/{path}\n+++ b/{path}\n@@\n");
    for line in truncate_text(old, 2_000).lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in truncate_text(new, 2_000).lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff.trim_end().to_string()
}

fn maybe_push_file_change_tool(
    entries: &mut Vec<TranscriptEntry>,
    call_id: Option<&str>,
    name: &str,
    input: &Value,
) -> bool {
    match name {
        "update_plan" => {
            let Some(plan) = input.get("plan").and_then(Value::as_array) else {
                return false;
            };
            let lines = plan
                .iter()
                .filter_map(|item| {
                    let step = item.get("step").and_then(Value::as_str)?.trim();
                    if step.is_empty() {
                        return None;
                    }
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending")
                        .trim();
                    Some(format!("{status}: {step}"))
                })
                .collect::<Vec<_>>();
            if lines.is_empty() {
                return false;
            }
            let completed = plan
                .iter()
                .filter(|item| item.get("status").and_then(Value::as_str) == Some("completed"))
                .count();
            let in_progress = plan
                .iter()
                .filter(|item| item.get("status").and_then(Value::as_str) == Some("in_progress"))
                .count();
            let mut metadata = HashMap::new();
            metadata.insert("items".to_string(), plan.len().to_string());
            metadata.insert("completed".to_string(), completed.to_string());
            metadata.insert("inProgress".to_string(), in_progress.to_string());
            push_structured_entry(
                entries,
                "Info",
                "Plan update".to_string(),
                vec![TranscriptBlock {
                    id: call_id.map(str::to_string),
                    kind: TranscriptBlockKind::PlanUpdate,
                    text: Some(lines.join("\n")),
                    tool_name: Some(name.to_string()),
                    status: None,
                    metadata,
                }],
                call_id.map(|id| format!("plan-update-{id}")),
                None,
                None,
                4_000,
            );
            true
        }
        "apply_patch" => {
            let patch = string_field(input, &["content", "patch", "input"]).unwrap_or_default();
            if patch.is_empty() {
                return false;
            }
            let mut metadata = HashMap::new();
            let (additions, deletions) = diff_line_counts(patch);
            metadata.insert("additions".to_string(), additions.to_string());
            metadata.insert("deletions".to_string(), deletions.to_string());
            push_file_change_entry(
                entries,
                call_id.map(str::to_string),
                Some(name.to_string()),
                None,
                "patch",
                Some(patch.to_string()),
                None,
                metadata,
            );
            true
        }
        "Edit" | "StrReplace" => {
            let Some(path) = string_field(input, &["file_path", "path"]) else {
                return false;
            };
            let old = string_field(input, &["old_string", "old"]).unwrap_or_default();
            let new = string_field(input, &["new_string", "new"]).unwrap_or_default();
            let diff = if old.is_empty() && new.is_empty() {
                None
            } else {
                Some(synthetic_edit_diff(path, old, new))
            };
            let mut metadata =
                metadata_from_tool_input(input, &["replace_all", "block_until_ms", "timeout"]);
            if let Some(diff) = diff.as_deref() {
                let (additions, deletions) = diff_line_counts(diff);
                metadata.insert("additions".to_string(), additions.to_string());
                metadata.insert("deletions".to_string(), deletions.to_string());
            }
            push_file_change_entry(
                entries,
                call_id.map(str::to_string),
                Some(name.to_string()),
                Some(path.to_string()),
                "edit",
                diff,
                None,
                metadata,
            );
            true
        }
        "Write" => {
            let Some(path) = string_field(input, &["file_path", "path"]) else {
                return false;
            };
            let content = string_field(input, &["content", "contents"]).unwrap_or_default();
            let mut metadata = HashMap::new();
            metadata.insert("bytes".to_string(), content.len().to_string());
            push_file_change_entry(
                entries,
                call_id.map(str::to_string),
                Some(name.to_string()),
                Some(path.to_string()),
                "write",
                None,
                None,
                metadata,
            );
            true
        }
        _ => false,
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let collapsed = text.trim();
    if collapsed.chars().count() <= max_chars {
        return collapsed.to_string();
    }
    let mut out = collapsed
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

/// True only if `path` resolves (after following symlinks and `..` segments)
/// to a location inside `root`. Both sides are canonicalized so a value like
/// `~/.claude/projects/../../secret.jsonl` cannot pass a lexical prefix check.
/// Canonicalization requires the path to exist; a non-existent path (or an
/// unresolvable root) is treated as untrusted.
fn path_within_root(path: &Path, root: &Path) -> bool {
    match (fs::canonicalize(path), fs::canonicalize(root)) {
        (Ok(resolved), Ok(root)) => resolved.starts_with(&root),
        _ => false,
    }
}

fn trusted_provider_transcript_path(provider: TranscriptProvider, path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let adapter = provider.adapter();
    (adapter.path_matches)(path)
        && (adapter.trusted_roots)()
            .iter()
            .any(|root| path_within_root(path, root))
}

fn find_transcript_by_provider_id(
    provider: TranscriptProvider,
    cwd: &str,
    provider_id: &str,
) -> Option<PathBuf> {
    if provider_id.trim().is_empty() {
        return None;
    }
    (provider.adapter().find_by_id)(cwd, provider_id)
        .filter(|path| trusted_provider_transcript_path(provider, path))
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

fn resolve_provider_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

#[cfg(test)]
fn kiro_project_dir(cwd: &str) -> Option<PathBuf> {
    transcript_kiro::kiro_project_dir(cwd)
}

#[cfg(test)]
fn kimi_project_dir(cwd: &str) -> Option<PathBuf> {
    transcript_kimi::project_dir(cwd)
}

fn list_dirs_sorted_desc(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    dirs
}

fn list_files_with_extensions(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && has_extension(path, extensions))
        .collect()
}

fn walk_files_with_extensions(
    root: &Path,
    extensions: &[&str],
    max_results: usize,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max_results {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && has_extension(&path, extensions) {
                out.push(path);
                if out.len() >= max_results {
                    break;
                }
            }
        }
    }
    out
}

fn newest_file(files: Vec<PathBuf>) -> Option<PathBuf> {
    files
        .into_iter()
        .filter_map(|path| {
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn best_file_for_session(files: Vec<PathBuf>, created_at_ms: u64) -> Option<PathBuf> {
    if created_at_ms == 0 {
        return newest_file(files);
    }
    files
        .into_iter()
        .filter_map(|path| {
            let stamp = file_created_or_modified_ms(&path)?;
            Some((stamp.abs_diff(created_at_ms), path))
        })
        .min_by_key(|(delta, _)| *delta)
        .map(|(_, path)| path)
}

fn file_created_or_modified_ms(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let time = metadata.created().or_else(|_| metadata.modified()).ok()?;
    system_time_ms(time)
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    Some(time.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn read_head_utf8(path: &Path, max_bytes: u64) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let mut buf = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(max_bytes)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ch if ch.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn flag_value(words: &[String], flags: &[&str]) -> Option<String> {
    for index in 0..words.len() {
        let word = words[index].as_str();
        if flags.contains(&word) {
            return words
                .get(index + 1)
                .filter(|value| !value.starts_with('-'))
                .cloned();
        }
        for flag in flags {
            let prefix = format!("{flag}=");
            if let Some(value) = word.strip_prefix(&prefix) {
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn flag_u64(args: &[String], flag: &str) -> Option<u64> {
    flag_string(args, flag)?.parse().ok()
}

fn flag_usize(args: &[String], flag: &str) -> Option<usize> {
    flag_string(args, flag)?.parse().ok()
}

fn flag_string(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    extensions.contains(&ext)
}

fn file_name_contains(path: &Path, needle: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().contains(needle))
}

#[cfg(test)]
mod tests;
