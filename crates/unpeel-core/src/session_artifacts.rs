//! Shared per-session gallery storage.
//!
//! Listing, original-byte reads, resumable upload, and deletion are portable
//! filesystem semantics and must not drift between Host implementations.
//! Native `max_dim` thumbnail generation remains an ImageIO-backed adapter
//! enrichment derived in memory from bytes supplied by this module.

use std::path::{Path, PathBuf};
#[cfg(not(unix))]
use std::time::UNIX_EPOCH;

#[cfg(unix)]
use serde::Deserialize;
use serde::Serialize;

#[cfg(unix)]
use sha2::{Digest, Sha256};

use crate::app_paths;

pub const LISTED_KINDS: &[&str] = &["screenshots", "downloads", "uploads", "computer"];

/// Raw bytes per artifact response. Once base64 encoded into the Controller
/// response and Relay-sealed, this remains below the 512 KiB frame ceiling.
pub const ARTIFACT_READ_MAX_CHUNK_BYTES: usize = 200 * 1024;

/// The largest raw image chunk accepted by the resumable upload protocol.
pub const RESUMABLE_UPLOAD_MAX_CHUNK_BYTES: usize = 256 * 1024;
/// The largest complete image accepted by the resumable upload protocol.
pub const RESUMABLE_UPLOAD_MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024;
/// Per-session bound for uploads which have not reached durable publication.
pub const RESUMABLE_UPLOAD_MAX_ACTIVE: usize = 8;
/// Per-session bound for the declared sizes of incomplete uploads.
pub const RESUMABLE_UPLOAD_MAX_STAGED_BYTES: u64 = 16 * 1024 * 1024;
/// Incomplete uploads with no accepted activity for this long are abandoned.
pub const RESUMABLE_UPLOAD_INCOMPLETE_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

/// Local Sessions MCP publication is not constrained by Controller frame
/// sizes, but it is still bounded so one tool call cannot read an arbitrary
/// large file into the Host process.
pub const LOCAL_GALLERY_IMAGE_MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedSessionImage {
    pub path: PathBuf,
    pub name: String,
    pub content_type: &'static str,
    pub size: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ResumableArtifactUploadRequest<'a> {
    pub session_id: &'a str,
    pub upload_id: &'a str,
    pub offset: u64,
    pub total_size: u64,
    pub sha256: &'a str,
    /// Normalized MIME type. Only `image/png` and `image/jpeg` are accepted.
    pub content_type: &'a str,
    /// Stable authenticated Controller identity. Only its SHA-256 is persisted.
    pub principal: &'a str,
    pub bytes: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableArtifactUploadResult {
    pub upload_id: String,
    pub next_offset: u64,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumableArtifactUploadError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
}

impl std::fmt::Display for ResumableArtifactUploadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ResumableArtifactUploadError {}

fn upload_error(
    status: u16,
    code: &'static str,
    message: impl Into<String>,
) -> ResumableArtifactUploadError {
    ResumableArtifactUploadError {
        status,
        code,
        message: message.into(),
        next_offset: None,
    }
}

fn upload_conflict(
    message: impl Into<String>,
    next_offset: Option<u64>,
) -> ResumableArtifactUploadError {
    ResumableArtifactUploadError {
        status: 409,
        code: "upload_conflict",
        message: message.into(),
        next_offset,
    }
}

/// Accept one idempotent chunk and publish the image into the Session's
/// `artifacts/uploads` directory once the complete digest and file signature
/// have been verified.
pub fn upload_resumable_artifact_chunk(
    request: ResumableArtifactUploadRequest<'_>,
) -> Result<ResumableArtifactUploadResult, ResumableArtifactUploadError> {
    #[cfg(unix)]
    {
        upload_resumable_artifact_chunk_at(&app_paths::app_sessions_root(), request)
    }
    #[cfg(not(unix))]
    {
        let _ = validate_upload_request(request)?;
        Err(upload_error(
            501,
            "upload_unsupported",
            "resumable artifact uploads are unsupported on this platform",
        ))
    }
}

/// Copy one local image into the calling Session's gallery. This is the
/// filesystem implementation behind Sessions MCP `add_to_gallery`: source
/// paths may point anywhere the hosted user can read, while publication is
/// confined to a verified Host-owned Session and a no-symlink artifact tree.
pub fn publish_local_image(
    session_id: &str,
    source: &Path,
) -> Result<PublishedSessionImage, String> {
    #[cfg(unix)]
    {
        publish_local_image_at(&app_paths::app_sessions_root(), session_id, source)
    }
    #[cfg(not(unix))]
    {
        let _ = (session_id, source);
        Err("adding local images to the gallery is unsupported on this platform".into())
    }
}

#[cfg(unix)]
fn publish_local_image_at(
    sessions_root: &Path,
    session_id: &str,
    source: &Path,
) -> Result<PublishedSessionImage, String> {
    use std::io::Read;

    if !safe_segment(session_id) {
        return Err("invalid session id".into());
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "image path must end in .png, .jpg, .jpeg, .gif, or .webp".to_string())?;
    let content_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return Err("gallery images must be PNG, JPEG, GIF, or WebP".into()),
    };

    let file = std::fs::File::open(source)
        .map_err(|error| format!("failed to open image {}: {error}", source.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect image {}: {error}", source.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "image path is not a regular file: {}",
            source.display()
        ));
    }
    if metadata.len() == 0 {
        return Err("gallery image is empty".into());
    }
    if metadata.len() > LOCAL_GALLERY_IMAGE_MAX_BYTES {
        return Err(format!(
            "gallery image is too large (maximum {} MiB)",
            LOCAL_GALLERY_IMAGE_MAX_BYTES / (1024 * 1024)
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(LOCAL_GALLERY_IMAGE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read image {}: {error}", source.display()))?;
    if bytes.len() as u64 > LOCAL_GALLERY_IMAGE_MAX_BYTES {
        return Err(format!(
            "gallery image is too large (maximum {} MiB)",
            LOCAL_GALLERY_IMAGE_MAX_BYTES / (1024 * 1024)
        ));
    }
    if !local_image_signature_matches(content_type, &bytes) {
        return Err(format!(
            "file contents do not match the {content_type} extension"
        ));
    }

    let sessions = secure_fs::open_configured_root(sessions_root)
        .map_err(|_| "session not found".to_string())?;
    let session = secure_fs::open_dir_at(&sessions, session_id)
        .map_err(|_| "session not found".to_string())?;
    verify_session_manifest(&session, session_id).map_err(|error| error.message)?;
    let artifacts = secure_fs::open_or_create_dir_at(&session, "artifacts")
        .map_err(|error| format!("failed to open gallery: {error}"))?;
    let uploads = secure_fs::open_or_create_dir_at(&artifacts, "uploads")
        .map_err(|error| format!("failed to open gallery uploads: {error}"))?;

    let source_stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .map(gallery_name_stem)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "image".into());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = format!(
        "{source_stem}-{}.{}",
        &id[..12],
        normalized_image_extension(content_type)
    );
    secure_fs::atomic_write_regular_at(&uploads, name.as_bytes(), &bytes)
        .map_err(|error| format!("failed to publish gallery image: {error}"))?;

    Ok(PublishedSessionImage {
        path: sessions_root
            .join(session_id)
            .join("artifacts")
            .join("uploads")
            .join(&name),
        name,
        content_type,
        size: bytes.len() as u64,
    })
}

#[cfg(unix)]
fn normalized_image_extension(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

#[cfg(unix)]
fn gallery_name_stem(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_separator = false;
    for character in value.chars().take(80) {
        if character.is_alphanumeric() || matches!(character, '-' | '_') {
            out.push(character);
            last_was_separator = false;
        } else if !last_was_separator && !out.is_empty() {
            out.push('-');
            last_was_separator = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(unix)]
fn local_image_signature_matches(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ValidatedUpload<'a> {
    session_id: &'a str,
    upload_id: &'a str,
    offset: u64,
    total_size: u64,
    sha256: &'a str,
    content_type: &'static str,
    bytes: &'a [u8],
    principal_sha256: String,
    upload_key: String,
    final_name: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResumableUploadState {
    version: u8,
    upload_id: String,
    principal_sha256: String,
    total_size: u64,
    sha256: String,
    content_type: String,
    final_name: String,
    #[serde(default)]
    committed_offset: u64,
    #[serde(default)]
    updated_at_unix_ms: u64,
    complete: bool,
    #[serde(default)]
    failed: bool,
}

#[cfg(unix)]
impl ResumableUploadState {
    fn new(upload: &ValidatedUpload<'_>, now_unix_ms: u64) -> Self {
        Self {
            version: 1,
            upload_id: upload.upload_id.to_owned(),
            principal_sha256: upload.principal_sha256.clone(),
            total_size: upload.total_size,
            sha256: upload.sha256.to_owned(),
            content_type: upload.content_type.to_owned(),
            final_name: upload.final_name.clone(),
            committed_offset: 0,
            updated_at_unix_ms: now_unix_ms,
            complete: false,
            failed: false,
        }
    }

    fn matches(&self, upload: &ValidatedUpload<'_>) -> bool {
        self.version == 1
            && self.upload_id == upload.upload_id
            && self.principal_sha256 == upload.principal_sha256
            && self.total_size == upload.total_size
            && self.sha256 == upload.sha256
            && self.content_type == upload.content_type
            && self.final_name == upload.final_name
    }
}

#[cfg(unix)]
fn upload_now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn validate_upload_request(
    request: ResumableArtifactUploadRequest<'_>,
) -> Result<ValidatedUpload<'_>, ResumableArtifactUploadError> {
    if !safe_segment(request.session_id) {
        return Err(upload_error(
            400,
            "invalid_session_id",
            "invalid session id",
        ));
    }
    let parsed_upload_id = uuid::Uuid::parse_str(request.upload_id).map_err(|_| {
        upload_error(
            400,
            "invalid_upload_id",
            "upload_id must be a canonical lowercase UUIDv4",
        )
    })?;
    if parsed_upload_id.to_string() != request.upload_id
        || parsed_upload_id.as_bytes()[6] >> 4 != 4
        || parsed_upload_id.as_bytes()[8] >> 6 != 2
    {
        return Err(upload_error(
            400,
            "invalid_upload_id",
            "upload_id must be a canonical lowercase UUIDv4",
        ));
    }
    if request.sha256.len() != 64
        || !request
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(upload_error(
            400,
            "invalid_sha256",
            "sha256 must be 64 lowercase hexadecimal characters",
        ));
    }
    let (content_type, extension) = match request.content_type {
        "image/png" => ("image/png", "png"),
        "image/jpeg" => ("image/jpeg", "jpg"),
        _ => {
            return Err(upload_error(
                415,
                "unsupported_media_type",
                "content type must be image/png or image/jpeg",
            ))
        }
    };
    if request.principal.trim().is_empty() || request.principal.len() > 1_024 {
        return Err(upload_error(
            400,
            "invalid_principal",
            "a stable authenticated principal is required",
        ));
    }
    if request.bytes.is_empty() {
        return Err(upload_error(400, "empty_chunk", "upload chunk is empty"));
    }
    if request.bytes.len() > RESUMABLE_UPLOAD_MAX_CHUNK_BYTES {
        return Err(upload_error(
            413,
            "chunk_too_large",
            format!("upload chunks may not exceed {RESUMABLE_UPLOAD_MAX_CHUNK_BYTES} bytes"),
        ));
    }
    if request.total_size == 0 || request.total_size > RESUMABLE_UPLOAD_MAX_TOTAL_BYTES {
        return Err(upload_error(
            413,
            "upload_too_large",
            format!("complete uploads may not exceed {RESUMABLE_UPLOAD_MAX_TOTAL_BYTES} bytes"),
        ));
    }
    let chunk_size = u64::try_from(request.bytes.len())
        .map_err(|_| upload_error(413, "chunk_too_large", "upload chunk size is unsupported"))?;
    let chunk_end = request
        .offset
        .checked_add(chunk_size)
        .ok_or_else(|| upload_error(400, "invalid_range", "upload chunk range overflows"))?;
    if request.offset >= request.total_size || chunk_end > request.total_size {
        return Err(upload_error(
            400,
            "invalid_range",
            "upload chunk falls outside total_size",
        ));
    }

    let principal_sha256 = sha256_hex(request.principal.as_bytes());
    let upload_key = sha256_hex(request.upload_id.as_bytes());
    let final_name = format!("upload-{}.{}", &upload_key[..32], extension);
    Ok(ValidatedUpload {
        session_id: request.session_id,
        upload_id: request.upload_id,
        offset: request.offset,
        total_size: request.total_size,
        sha256: request.sha256,
        content_type,
        bytes: request.bytes,
        principal_sha256,
        upload_key,
        final_name,
    })
}

#[cfg(not(unix))]
fn validate_upload_request(
    request: ResumableArtifactUploadRequest<'_>,
) -> Result<(), ResumableArtifactUploadError> {
    if request.bytes.is_empty() {
        return Err(upload_error(400, "empty_chunk", "upload chunk is empty"));
    }
    if request.bytes.len() > RESUMABLE_UPLOAD_MAX_CHUNK_BYTES
        || request.total_size > RESUMABLE_UPLOAD_MAX_TOTAL_BYTES
    {
        return Err(upload_error(
            413,
            "upload_too_large",
            "upload exceeds its size limit",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(unix)]
fn upload_resumable_artifact_chunk_at(
    sessions_root: &Path,
    request: ResumableArtifactUploadRequest<'_>,
) -> Result<ResumableArtifactUploadResult, ResumableArtifactUploadError> {
    let upload = validate_upload_request(request)?;
    let sessions = secure_fs::open_configured_root(sessions_root)
        .map_err(|_| upload_error(404, "session_not_found", "session not found"))?;
    let session = secure_fs::open_dir_at(&sessions, upload.session_id)
        .map_err(|_| upload_error(404, "session_not_found", "session not found"))?;
    verify_session_manifest(&session, upload.session_id)?;

    // These are the only directories the upload endpoint may create, and only
    // after a real Host-owned Session has been verified through its manifest.
    let artifacts = secure_fs::open_or_create_dir_at(&session, "artifacts")
        .map_err(|error| upload_storage_error("open artifacts directory", error))?;
    let uploads = secure_fs::open_or_create_dir_at(&artifacts, "uploads")
        .map_err(|error| upload_storage_error("open uploads directory", error))?;
    process_upload_chunk(sessions_root, &uploads, &upload)
}

#[cfg(unix)]
fn verify_session_manifest(
    session: &std::fs::File,
    session_id: &str,
) -> Result<(), ResumableArtifactUploadError> {
    let raw = secure_fs::read_regular_at(session, b"manifest.json", 2 * 1024 * 1024)
        .map_err(|_| upload_error(404, "session_not_found", "session not found"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|_| upload_error(404, "session_not_found", "session not found"))?;
    if manifest
        .get("session")
        .and_then(|session| session.get("id"))
        .and_then(serde_json::Value::as_str)
        != Some(session_id)
    {
        return Err(upload_error(404, "session_not_found", "session not found"));
    }
    Ok(())
}

#[cfg(unix)]
fn upload_storage_error(context: &str, error: std::io::Error) -> ResumableArtifactUploadError {
    if matches!(
        error.raw_os_error(),
        Some(code) if code == libc::ENOSPC || code == libc::EDQUOT
    ) {
        return upload_error(
            507,
            "insufficient_storage",
            format!("{context}: Host storage is full"),
        );
    }
    upload_error(500, "upload_storage_error", format!("{context}: {error}"))
}

#[cfg(unix)]
fn upload_state_names(upload: &ValidatedUpload<'_>) -> (String, String) {
    (
        format!(".unpeel-upload-{}.json", upload.upload_key),
        format!(".unpeel-upload-{}.part", upload.upload_key),
    )
}

#[cfg(unix)]
fn process_upload_chunk(
    sessions_root: &Path,
    uploads: &std::fs::File,
    upload: &ValidatedUpload<'_>,
) -> Result<ResumableArtifactUploadResult, ResumableArtifactUploadError> {
    use std::io::{Read, Seek, SeekFrom, Write};

    // One per-Session lock serializes chunk commits, quota decisions, and
    // expiry cleanup. Uploads are small and network-bound; avoiding a second
    // lock order is more valuable than parallel writes within one Session.
    let _upload_lock = secure_fs::open_and_lock_file(uploads, b".unpeel-upload.lock")
        .map_err(|error| upload_storage_error("lock upload", error))?;
    let now_unix_ms = upload_now_unix_ms();
    recover_expired_uploads(uploads)?;
    expire_incomplete_uploads(uploads, now_unix_ms)?;
    let (state_name, part_name) = upload_state_names(upload);

    let mut state = match secure_fs::read_regular_at(uploads, state_name.as_bytes(), 64 * 1024) {
        Ok(raw) => {
            let state: ResumableUploadState = serde_json::from_slice(&raw)
                .map_err(|_| upload_conflict("upload state is invalid", None))?;
            if !state.matches(upload) {
                return Err(upload_conflict(
                    "upload_id is already bound to different metadata or principal",
                    Some(state.committed_offset),
                ));
            }
            state
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if upload.offset != 0 {
                return Err(upload_conflict(
                    "a new upload must begin at offset zero",
                    Some(0),
                ));
            }
            if secure_entry_exists(uploads, part_name.as_bytes())?
                || secure_entry_exists(uploads, upload.final_name.as_bytes())?
            {
                return Err(upload_conflict(
                    "upload_id has orphaned storage state",
                    None,
                ));
            }
            enforce_upload_quota(uploads, upload.total_size)?;
            let state = ResumableUploadState::new(upload, now_unix_ms);
            persist_upload_state(uploads, state_name.as_bytes(), &state)?;
            state
        }
        Err(error) => return Err(upload_storage_error("read upload state", error)),
    };

    if state.committed_offset > state.total_size {
        return Err(upload_conflict(
            "upload state has an invalid committed offset",
            Some(state.committed_offset),
        ));
    }
    if state.failed {
        match secure_fs::metadata_at(uploads, part_name.as_bytes()) {
            Ok(metadata) if metadata.regular_file => {
                secure_fs::unlink_regular_at(uploads, part_name.as_bytes())
                    .map_err(|error| upload_storage_error("remove rejected upload", error))?;
            }
            Ok(_) => {
                return Err(upload_conflict(
                    "rejected upload staging state is not a regular file",
                    Some(state.committed_offset),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(upload_storage_error("inspect rejected upload", error)),
        }
        return Err(upload_conflict(
            "upload_id previously failed integrity validation",
            Some(state.committed_offset),
        ));
    }

    match secure_regular_size(uploads, upload.final_name.as_bytes())? {
        Some(_) => {
            return recover_published_upload(
                sessions_root,
                uploads,
                upload,
                &state_name,
                &part_name,
                &mut state,
            )
        }
        None if state.complete => {
            return Err(upload_conflict(
                "published upload is missing",
                Some(upload.total_size),
            ))
        }
        None => {}
    }

    let mut part = secure_fs::open_regular_rw_create_at(uploads, part_name.as_bytes())
        .map_err(|error| upload_storage_error("open upload staging file", error))?;
    let physical_size = part
        .metadata()
        .map_err(|error| upload_storage_error("inspect upload staging file", error))?
        .len();
    if physical_size < state.committed_offset {
        return Err(upload_conflict(
            "staged upload is shorter than its committed offset",
            Some(physical_size),
        ));
    }
    if physical_size > state.committed_offset {
        // The process can die after the kernel accepts part of a chunk but
        // before the metadata commit. That tail was never acknowledged.
        part.set_len(state.committed_offset)
            .map_err(|error| upload_storage_error("trim uncommitted upload tail", error))?;
        part.sync_all()
            .map_err(|error| upload_storage_error("sync trimmed upload tail", error))?;
    }
    let current_size = state.committed_offset;

    let chunk_end = upload.offset + upload.bytes.len() as u64;
    if upload.offset > current_size {
        return Err(upload_conflict(
            "upload chunk begins after the next accepted offset",
            Some(current_size),
        ));
    }
    if upload.offset < current_size {
        if chunk_end > current_size {
            return Err(upload_conflict(
                "upload chunk partially overlaps staged data",
                Some(current_size),
            ));
        }
        part.seek(SeekFrom::Start(upload.offset))
            .map_err(|error| upload_storage_error("seek staged upload", error))?;
        let mut existing = vec![0_u8; upload.bytes.len()];
        part.read_exact(&mut existing)
            .map_err(|error| upload_storage_error("read staged upload", error))?;
        if existing != upload.bytes {
            return Err(upload_conflict(
                "repeated upload range contains different bytes",
                Some(current_size),
            ));
        }
        state.updated_at_unix_ms = now_unix_ms;
        persist_upload_state(uploads, state_name.as_bytes(), &state)?;
    } else {
        part.seek(SeekFrom::Start(current_size))
            .map_err(|error| upload_storage_error("seek staged upload", error))?;
        part.write_all(upload.bytes)
            .map_err(|error| upload_storage_error("write upload chunk", error))?;
        part.sync_all()
            .map_err(|error| upload_storage_error("sync upload chunk", error))?;
        state.committed_offset = chunk_end;
        state.updated_at_unix_ms = now_unix_ms;
        // The Controller only learns next_offset after both bytes and this
        // commit marker are durable. On an uncertain failure, the same range
        // can therefore be retried without accepting a partial write.
        persist_upload_state(uploads, state_name.as_bytes(), &state)?;
    }

    let next_offset = if upload.offset == current_size {
        chunk_end
    } else {
        current_size
    };
    if next_offset < upload.total_size {
        return Ok(ResumableArtifactUploadResult {
            upload_id: upload.upload_id.to_owned(),
            next_offset,
            complete: false,
            path: None,
            name: None,
            content_type: None,
            sha256: None,
        });
    }

    if let Err(error) = validate_complete_file(&mut part, upload, false) {
        if error.code == "upload_digest_mismatch"
            || error.code == "unsupported_media_type"
            || error.code == "upload_integrity_failed"
        {
            mark_upload_failed(
                uploads,
                state_name.as_bytes(),
                part_name.as_bytes(),
                &mut state,
            )?;
        }
        return Err(error);
    }
    match secure_fs::link_regular_at(uploads, part_name.as_bytes(), upload.final_name.as_bytes()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut published =
                secure_fs::open_regular_read_at(uploads, upload.final_name.as_bytes())
                    .map_err(|error| upload_storage_error("open published upload", error))?;
            validate_complete_file(&mut published, upload, true)?;
        }
        Err(error) => return Err(upload_storage_error("publish upload", error)),
    }
    secure_fs::unlink_regular_at(uploads, part_name.as_bytes())
        .map_err(|error| upload_storage_error("remove upload staging file", error))?;
    secure_fs::sync_directory(uploads)
        .map_err(|error| upload_storage_error("sync uploads directory", error))?;
    state.committed_offset = upload.total_size;
    state.updated_at_unix_ms = now_unix_ms;
    state.complete = true;
    persist_upload_state(uploads, state_name.as_bytes(), &state)?;

    Ok(completed_upload_result(sessions_root, upload))
}

#[cfg(unix)]
fn secure_entry_exists(
    dir: &std::fs::File,
    name: &[u8],
) -> Result<bool, ResumableArtifactUploadError> {
    match secure_fs::metadata_at(dir, name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(upload_storage_error("inspect upload state", error)),
    }
}

#[cfg(unix)]
fn secure_regular_size(
    dir: &std::fs::File,
    name: &[u8],
) -> Result<Option<u64>, ResumableArtifactUploadError> {
    match secure_fs::metadata_at(dir, name) {
        Ok(metadata) if metadata.regular_file => Ok(Some(metadata.size)),
        Ok(_) => Err(upload_conflict(
            "upload storage entry is not a regular file",
            None,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(upload_storage_error("inspect upload storage entry", error)),
    }
}

#[cfg(unix)]
fn upload_key_from_internal_name(name: &[u8], suffix: &str) -> Option<String> {
    let rendered = std::str::from_utf8(name).ok()?;
    let key = rendered
        .strip_prefix(".unpeel-upload-")?
        .strip_suffix(suffix)?;
    if key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Some(key.to_owned())
    } else {
        None
    }
}

#[cfg(unix)]
fn recover_expired_uploads(uploads: &std::fs::File) -> Result<(), ResumableArtifactUploadError> {
    let names = secure_fs::entry_names(uploads)
        .map_err(|error| upload_storage_error("scan expired uploads", error))?;
    let mut changed = false;
    for name in names {
        let Some(key) = upload_key_from_internal_name(&name, ".expired") else {
            continue;
        };
        let part_name = format!(".unpeel-upload-{key}.part");
        secure_fs::unlink_regular_or_symlink(uploads, part_name.as_bytes())
            .map_err(|error| upload_storage_error("recover expired upload bytes", error))?;
        secure_fs::unlink_regular_or_symlink(uploads, &name)
            .map_err(|error| upload_storage_error("recover expired upload receipt", error))?;
        changed = true;
    }
    if changed {
        secure_fs::sync_directory(uploads)
            .map_err(|error| upload_storage_error("sync expired upload recovery", error))?;
    }
    Ok(())
}

#[cfg(unix)]
fn expire_incomplete_uploads(
    uploads: &std::fs::File,
    now_unix_ms: u64,
) -> Result<(), ResumableArtifactUploadError> {
    let names = secure_fs::entry_names(uploads)
        .map_err(|error| upload_storage_error("scan incomplete uploads", error))?;
    for state_name in names {
        let Some(key) = upload_key_from_internal_name(&state_name, ".json") else {
            continue;
        };
        let Ok(raw) = secure_fs::read_regular_at(uploads, &state_name, 64 * 1024) else {
            // Corrupt or non-regular receipts remain conservative quota users;
            // without a valid state record we cannot prove they are incomplete.
            continue;
        };
        let Ok(state) = serde_json::from_slice::<ResumableUploadState>(&raw) else {
            continue;
        };
        if state.complete || state.failed {
            continue;
        }
        let expired = state.updated_at_unix_ms == 0
            || now_unix_ms.saturating_sub(state.updated_at_unix_ms)
                >= RESUMABLE_UPLOAD_INCOMPLETE_TTL_MS;
        if !expired {
            continue;
        }

        // Renaming the receipt is the durable expiry commit. If the process
        // dies after this point, recover_expired_uploads finishes removing the
        // matching part before any request examines upload state.
        let expired_name = format!(".unpeel-upload-{key}.expired");
        secure_fs::rename_regular_at(uploads, &state_name, expired_name.as_bytes())
            .map_err(|error| upload_storage_error("expire upload receipt", error))?;
        let part_name = format!(".unpeel-upload-{key}.part");
        secure_fs::unlink_regular_or_symlink(uploads, part_name.as_bytes())
            .map_err(|error| upload_storage_error("expire upload bytes", error))?;
        secure_fs::unlink_regular_or_symlink(uploads, expired_name.as_bytes())
            .map_err(|error| upload_storage_error("finish upload expiry", error))?;
        secure_fs::sync_directory(uploads)
            .map_err(|error| upload_storage_error("sync upload expiry", error))?;
    }
    Ok(())
}

#[cfg(unix)]
fn enforce_upload_quota(
    uploads: &std::fs::File,
    new_total_size: u64,
) -> Result<(), ResumableArtifactUploadError> {
    let names = secure_fs::entry_names(uploads)
        .map_err(|error| upload_storage_error("read upload quota state", error))?;
    let mut active = 0_usize;
    let mut staged_bytes = 0_u64;
    for name in names {
        let rendered = String::from_utf8_lossy(&name);
        if !rendered.starts_with(".unpeel-upload-") || !rendered.ends_with(".json") {
            continue;
        }
        let parsed = secure_fs::read_regular_at(uploads, &name, 64 * 1024)
            .ok()
            .and_then(|raw| serde_json::from_slice::<ResumableUploadState>(&raw).ok());
        match parsed {
            Some(state) if state.complete || state.failed => {
                // Failed receipts stay bound just like successful receipts so
                // losing a terminal 415/422 response cannot make the same id
                // mean something else on retry. This is not a new practical
                // storage-amplification boundary: only an authenticated
                // artifact writer can create one, and that principal can
                // already publish much larger gallery artifacts. Any future
                // pruning therefore belongs to the Session artifact-retention
                // policy, where completed files and their receipts can expire
                // together without weakening idempotency in isolation.
            }
            Some(state) => {
                active = active.saturating_add(1);
                staged_bytes = staged_bytes.saturating_add(state.total_size);
            }
            None => {
                // Corrupt hidden state must not let new ids bypass the bound.
                active = active.saturating_add(1);
                staged_bytes = staged_bytes.saturating_add(RESUMABLE_UPLOAD_MAX_TOTAL_BYTES);
            }
        }
    }
    if active >= RESUMABLE_UPLOAD_MAX_ACTIVE
        || staged_bytes.saturating_add(new_total_size) > RESUMABLE_UPLOAD_MAX_STAGED_BYTES
    {
        return Err(upload_error(
            429,
            "upload_quota_exceeded",
            "too many incomplete uploads for this session",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn persist_upload_state(
    uploads: &std::fs::File,
    state_name: &[u8],
    state: &ResumableUploadState,
) -> Result<(), ResumableArtifactUploadError> {
    let encoded = serde_json::to_vec(state)
        .map_err(|error| upload_error(500, "upload_storage_error", error.to_string()))?;
    secure_fs::atomic_write_regular_at(uploads, state_name, &encoded)
        .map_err(|error| upload_storage_error("persist upload state", error))
}

#[cfg(unix)]
fn recover_published_upload(
    sessions_root: &Path,
    uploads: &std::fs::File,
    upload: &ValidatedUpload<'_>,
    state_name: &str,
    part_name: &str,
    state: &mut ResumableUploadState,
) -> Result<ResumableArtifactUploadResult, ResumableArtifactUploadError> {
    let mut published = secure_fs::open_regular_read_at(uploads, upload.final_name.as_bytes())
        .map_err(|error| upload_storage_error("open published upload", error))?;
    validate_complete_file(&mut published, upload, true)?;
    match secure_fs::metadata_at(uploads, part_name.as_bytes()) {
        Ok(metadata) if metadata.regular_file => {
            secure_fs::unlink_regular_at(uploads, part_name.as_bytes())
                .map_err(|error| upload_storage_error("remove recovered staging file", error))?;
        }
        Ok(_) => {
            return Err(upload_conflict(
                "upload staging state is not a regular file",
                Some(upload.total_size),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(upload_storage_error("inspect upload staging file", error)),
    }
    if !state.complete {
        state.committed_offset = upload.total_size;
        state.updated_at_unix_ms = upload_now_unix_ms();
        state.complete = true;
        persist_upload_state(uploads, state_name.as_bytes(), state)?;
    }
    Ok(completed_upload_result(sessions_root, upload))
}

#[cfg(unix)]
fn validate_complete_file(
    file: &mut std::fs::File,
    upload: &ValidatedUpload<'_>,
    published: bool,
) -> Result<(), ResumableArtifactUploadError> {
    use std::io::{Read, Seek, SeekFrom};

    let length = file
        .metadata()
        .map_err(|error| upload_storage_error("inspect complete upload", error))?
        .len();
    if length != upload.total_size {
        return if published {
            Err(upload_conflict(
                "published upload has an unexpected size",
                Some(length),
            ))
        } else {
            Err(upload_error(
                422,
                "upload_integrity_failed",
                "complete upload has an unexpected size",
            ))
        };
    }

    file.seek(SeekFrom::Start(0))
        .map_err(|error| upload_storage_error("seek complete upload", error))?;
    let mut hasher = Sha256::new();
    let mut signature = [0_u8; 8];
    let mut signature_length = 0_usize;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| upload_storage_error("read complete upload", error))?;
        if read == 0 {
            break;
        }
        if signature_length < signature.len() {
            let copy = (signature.len() - signature_length).min(read);
            signature[signature_length..signature_length + copy].copy_from_slice(&buffer[..copy]);
            signature_length += copy;
        }
        hasher.update(&buffer[..read]);
    }
    let actual_digest = {
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    };
    if actual_digest != upload.sha256 {
        return if published {
            Err(upload_conflict(
                "published upload does not match its digest",
                Some(upload.total_size),
            ))
        } else {
            Err(upload_error(
                422,
                "upload_digest_mismatch",
                "complete upload does not match sha256",
            ))
        };
    }
    let signature_matches = match upload.content_type {
        "image/png" => {
            signature_length >= 8 && signature == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        }
        "image/jpeg" => {
            signature_length >= 3
                && signature[0] == 0xff
                && signature[1] == 0xd8
                && signature[2] == 0xff
        }
        _ => false,
    };
    if !signature_matches {
        return if published {
            Err(upload_conflict(
                "published upload does not match its image type",
                Some(upload.total_size),
            ))
        } else {
            Err(upload_error(
                415,
                "unsupported_media_type",
                "complete upload does not match its image type",
            ))
        };
    }

    file.seek(SeekFrom::Start(upload.offset))
        .map_err(|error| upload_storage_error("seek repeated upload range", error))?;
    let mut repeated = vec![0_u8; upload.bytes.len()];
    file.read_exact(&mut repeated)
        .map_err(|error| upload_storage_error("read repeated upload range", error))?;
    if repeated != upload.bytes {
        return Err(upload_conflict(
            "repeated upload range contains different bytes",
            Some(upload.total_size),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn mark_upload_failed(
    uploads: &std::fs::File,
    state_name: &[u8],
    part_name: &[u8],
    state: &mut ResumableUploadState,
) -> Result<(), ResumableArtifactUploadError> {
    state.failed = true;
    // Commit the terminal receipt before removing bytes so a crash cannot
    // leave this id permanently counted as active.
    state.updated_at_unix_ms = upload_now_unix_ms();
    persist_upload_state(uploads, state_name, state)?;
    secure_fs::unlink_regular_at(uploads, part_name)
        .map_err(|error| upload_storage_error("remove rejected upload", error))?;
    secure_fs::sync_directory(uploads)
        .map_err(|error| upload_storage_error("sync rejected upload cleanup", error))
}

#[cfg(unix)]
fn completed_upload_result(
    sessions_root: &Path,
    upload: &ValidatedUpload<'_>,
) -> ResumableArtifactUploadResult {
    let path = sessions_root
        .join(upload.session_id)
        .join("artifacts")
        .join("uploads")
        .join(&upload.final_name);
    ResumableArtifactUploadResult {
        upload_id: upload.upload_id.to_owned(),
        next_offset: upload.total_size,
        complete: true,
        path: Some(path),
        name: Some(upload.final_name.clone()),
        content_type: Some(upload.content_type.to_owned()),
        sha256: Some(upload.sha256.to_owned()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionArtifactMetadata {
    pub kind: String,
    pub name: String,
    pub size: u64,
    pub modified_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArtifactChunk {
    pub kind: String,
    pub name: String,
    pub content_type: &'static str,
    pub offset: u64,
    pub next_offset: u64,
    pub total_size: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArtifactReadError {
    pub status: u16,
    pub message: &'static str,
}

impl std::fmt::Display for SessionArtifactReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for SessionArtifactReadError {}

fn read_error(status: u16, message: &'static str) -> SessionArtifactReadError {
    SessionArtifactReadError { status, message }
}

pub fn artifact_content_type(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("txt" | "log") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Read one bounded, offset-addressed slice. On Unix, the walk is no-follow
/// from the configured app-sessions root through the artifact leaf.
pub fn read_chunk(
    session_id: &str,
    kind: &str,
    name: &str,
    offset: u64,
    limit: usize,
) -> Result<SessionArtifactChunk, SessionArtifactReadError> {
    if !safe_segment(session_id) {
        return Err(read_error(400, "invalid session id"));
    }
    if !safe_segment(name) {
        return Err(read_error(400, "invalid artifact path"));
    }
    let Some(components) = kind_components(kind) else {
        return Err(read_error(404, "unknown artifact kind"));
    };
    let limit = limit.clamp(1, ARTIFACT_READ_MAX_CHUNK_BYTES);

    #[cfg(unix)]
    {
        let root = secure_fs::open_session_artifact_root(session_id)
            .map_err(|_| read_error(404, "unknown artifact"))?;
        read_chunk_opened_root(&root, components, kind, name, offset, limit)
    }
    #[cfg(not(unix))]
    {
        read_chunk_at(
            &artifacts_root(session_id),
            components,
            kind,
            name,
            offset,
            limit,
        )
    }
}

#[cfg(unix)]
fn read_chunk_opened_root(
    root: &std::fs::File,
    components: &[&str],
    kind: &str,
    name: &str,
    offset: u64,
    limit: usize,
) -> Result<SessionArtifactChunk, SessionArtifactReadError> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let dir = secure_fs::open_dir_chain(root, components)
        .map_err(|_| read_error(404, "unknown artifact"))?;
    let mut file = secure_fs::open_regular_read_at(&dir, name.as_bytes())
        .map_err(|_| read_error(404, "unknown artifact"))?;
    let total_size = file
        .metadata()
        .map_err(|_| read_error(500, "artifact read failed"))?
        .len();
    let start = offset.min(total_size);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| read_error(500, "artifact read failed"))?;
    let available = total_size.saturating_sub(start).min(limit as u64);
    let mut bytes = Vec::with_capacity(available as usize);
    file.take(available)
        .read_to_end(&mut bytes)
        .map_err(|_| read_error(500, "artifact read failed"))?;
    Ok(SessionArtifactChunk {
        kind: kind.to_owned(),
        name: name.to_owned(),
        content_type: artifact_content_type(name),
        offset: start,
        next_offset: start + bytes.len() as u64,
        total_size,
        bytes,
    })
}

#[cfg(all(unix, test))]
fn read_chunk_at(
    root: &Path,
    components: &[&str],
    kind: &str,
    name: &str,
    offset: u64,
    limit: usize,
) -> Result<SessionArtifactChunk, SessionArtifactReadError> {
    let root =
        secure_fs::open_root_for_test(root).map_err(|_| read_error(404, "unknown artifact"))?;
    read_chunk_opened_root(&root, components, kind, name, offset, limit)
}

#[cfg(not(unix))]
fn read_chunk_at(
    root: &Path,
    components: &[&str],
    kind: &str,
    name: &str,
    offset: u64,
    limit: usize,
) -> Result<SessionArtifactChunk, SessionArtifactReadError> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut dir = root.to_owned();
    for component in components {
        dir.push(component);
    }
    let path = dir.join(name);
    let metadata = path
        .symlink_metadata()
        .map_err(|_| read_error(404, "unknown artifact"))?;
    if !metadata.file_type().is_file() {
        return Err(read_error(404, "unknown artifact"));
    }
    let total_size = metadata.len();
    let start = offset.min(total_size);
    let mut file = std::fs::File::open(path).map_err(|_| read_error(404, "unknown artifact"))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|_| read_error(500, "artifact read failed"))?;
    let available = total_size.saturating_sub(start).min(limit as u64);
    let mut bytes = Vec::with_capacity(available as usize);
    file.take(available)
        .read_to_end(&mut bytes)
        .map_err(|_| read_error(500, "artifact read failed"))?;
    Ok(SessionArtifactChunk {
        kind: kind.to_owned(),
        name: name.to_owned(),
        content_type: artifact_content_type(name),
        offset: start,
        next_offset: start + bytes.len() as u64,
        total_size,
        bytes,
    })
}

fn artifacts_root(session_id: &str) -> PathBuf {
    app_paths::app_sessions_root()
        .join(session_id)
        .join("artifacts")
}

pub fn kind_dir(session_id: &str, kind: &str) -> Option<PathBuf> {
    if !safe_segment(session_id) {
        return None;
    }
    kind_dir_at(&artifacts_root(session_id), kind)
}

fn kind_dir_at(root: &Path, kind: &str) -> Option<PathBuf> {
    let mut path = root.to_owned();
    for component in kind_components(kind)? {
        path.push(component);
    }
    Some(path)
}

fn kind_components(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "screenshots" => Some(&["browser", "screenshots"]),
        "downloads" => Some(&["browser", "downloads"]),
        "uploads" => Some(&["uploads"]),
        "computer" => Some(&["computer", "screenshots"]),
        _ => None,
    }
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains("..")
        && !value.contains('\0')
}

pub fn list(session_id: &str) -> Vec<SessionArtifactMetadata> {
    if !safe_segment(session_id) {
        return Vec::new();
    }
    #[cfg(unix)]
    {
        let Ok(root) = secure_fs::open_session_artifact_root(session_id) else {
            return Vec::new();
        };
        list_opened_root(&root)
    }
    #[cfg(not(unix))]
    {
        list_at(&artifacts_root(session_id))
    }
}

#[cfg(not(unix))]
fn list_at(root: &Path) -> Vec<SessionArtifactMetadata> {
    let mut artifacts = Vec::new();
    for kind in LISTED_KINDS {
        let Some(dir) = kind_dir_at(root, kind) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            // Never follow a symlink out of the Host-owned artifact tree.
            let Ok(metadata) = entry.path().symlink_metadata() else {
                continue;
            };
            if !metadata.file_type().is_file() {
                continue;
            }
            let modified_at_unix_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_millis() as u64)
                .unwrap_or(0);
            artifacts.push(SessionArtifactMetadata {
                kind: (*kind).to_owned(),
                name,
                size: metadata.len(),
                modified_at_unix_ms,
            });
        }
    }
    artifacts.sort_by(|left, right| {
        right
            .modified_at_unix_ms
            .cmp(&left.modified_at_unix_ms)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    artifacts
}

#[cfg(all(unix, test))]
fn list_at(root: &Path) -> Vec<SessionArtifactMetadata> {
    let Ok(root) = secure_fs::open_root_for_test(root) else {
        return Vec::new();
    };
    list_opened_root(&root)
}

#[cfg(unix)]
fn list_opened_root(root: &std::fs::File) -> Vec<SessionArtifactMetadata> {
    let mut artifacts = Vec::new();
    for kind in LISTED_KINDS {
        let Some(components) = kind_components(kind) else {
            continue;
        };
        let Ok(dir) = secure_fs::open_dir_chain(root, components) else {
            continue;
        };
        let Ok(names) = secure_fs::entry_names(&dir) else {
            continue;
        };
        for raw_name in names {
            let name = String::from_utf8_lossy(&raw_name).into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(metadata) = secure_fs::metadata_at(&dir, &raw_name) else {
                continue;
            };
            if !metadata.regular_file {
                continue;
            }
            artifacts.push(SessionArtifactMetadata {
                kind: (*kind).to_owned(),
                name,
                size: metadata.size,
                modified_at_unix_ms: metadata.modified_at_unix_ms,
            });
        }
    }
    sort_artifacts(&mut artifacts);
    artifacts
}

fn sort_artifacts(artifacts: &mut [SessionArtifactMetadata]) {
    artifacts.sort_by(|left, right| {
        right
            .modified_at_unix_ms
            .cmp(&left.modified_at_unix_ms)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// Idempotently remove one artifact and every cached native thumbnail variant.
pub fn delete(session_id: &str, kind: &str, name: &str) -> Result<(), String> {
    if !safe_segment(session_id) {
        return Err("invalid session id".into());
    }
    if !safe_segment(name) {
        return Err("invalid artifact path".into());
    }
    let Some(components) = kind_components(kind) else {
        return Err("unknown artifact kind".into());
    };
    #[cfg(unix)]
    {
        let root = match secure_fs::open_session_artifact_root(session_id) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("open artifact root: {error}")),
        };
        delete_opened_root(&root, components, kind, name)
    }
    #[cfg(not(unix))]
    {
        delete_at(&artifacts_root(session_id), kind, name)
    }
}

#[cfg(not(unix))]
fn delete_at(root: &Path, kind: &str, name: &str) -> Result<(), String> {
    let dir = kind_dir_at(root, kind).ok_or_else(|| "unknown artifact kind".to_owned())?;
    let target = dir.join(name);
    match target.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            std::fs::remove_file(&target)
                .map_err(|error| format!("remove {}: {error}", target.display()))?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect {}: {error}", target.display())),
    }

    let suffix = format!("-{kind}-{name}.jpg");
    if let Ok(entries) = std::fs::read_dir(root.join("thumbs")) {
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().ends_with(&suffix) {
                continue;
            }
            let path = entry.path();
            let is_removable = path
                .symlink_metadata()
                .map(|metadata| metadata.file_type().is_file() || metadata.file_type().is_symlink())
                .unwrap_or(false);
            if is_removable {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}

#[cfg(all(unix, test))]
fn delete_at(root: &Path, kind: &str, name: &str) -> Result<(), String> {
    if !safe_segment(name) {
        return Err("invalid artifact path".into());
    }
    let components = kind_components(kind).ok_or_else(|| "unknown artifact kind".to_owned())?;
    let root = match secure_fs::open_root_for_test(root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("open artifact root: {error}")),
    };
    delete_opened_root(&root, components, kind, name)
}

#[cfg(unix)]
fn delete_opened_root(
    root: &std::fs::File,
    components: &[&str],
    kind: &str,
    name: &str,
) -> Result<(), String> {
    match secure_fs::open_dir_chain(root, components) {
        Ok(dir) => secure_fs::unlink_regular_or_symlink(&dir, name.as_bytes())
            .map_err(|error| format!("remove artifact: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("open artifact directory: {error}")),
    }

    let thumbs = match secure_fs::open_dir_chain(root, &["thumbs"]) {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("open thumbnail directory: {error}")),
    };
    let suffix = format!("-{kind}-{name}.jpg");
    if let Ok(names) = secure_fs::entry_names(&thumbs) {
        for raw_name in names {
            if !String::from_utf8_lossy(&raw_name).ends_with(&suffix) {
                continue;
            }
            let _ = secure_fs::unlink_regular_or_symlink(&thumbs, &raw_name);
        }
    }
    Ok(())
}

#[cfg(unix)]
mod secure_fs {
    use std::ffi::{CStr, CString};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    use super::{app_paths, safe_segment};

    pub struct Metadata {
        pub regular_file: bool,
        pub size: u64,
        pub modified_at_unix_ms: u64,
    }

    struct DirStream(*mut libc::DIR);

    impl Drop for DirStream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    fn open_dir(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    }

    pub fn open_dir_at(parent: &File, component: &str) -> io::Result<File> {
        if !safe_segment(component) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe path component",
            ));
        }
        open_dir_at_bytes(parent, component.as_bytes())
    }

    fn safe_leaf(name: &[u8]) -> io::Result<CString> {
        if name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&b'/')
            || name.contains(&b'\\')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe path component",
            ));
        }
        CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path component"))
    }

    fn open_dir_at_bytes(parent: &File, component: &[u8]) -> io::Result<File> {
        let component = safe_leaf(component)?;
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    /// Resolve configured/OS-owned parent components normally, while refusing
    /// to use a symlink as the app-sessions trust anchor itself. Every
    /// descendant is opened relative to this handle with O_NOFOLLOW.
    pub fn open_configured_root(path: &Path) -> io::Result<File> {
        open_dir(path)
    }

    pub fn open_or_create_dir_at(parent: &File, component: &str) -> io::Result<File> {
        if !safe_segment(component) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe path component",
            ));
        }
        let encoded = safe_leaf(component.as_bytes())?;
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), encoded.as_ptr(), 0o700) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        open_dir_at(parent, component)
    }

    pub fn open_session_artifact_root(session_id: &str) -> io::Result<File> {
        if !safe_segment(session_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid session id",
            ));
        }
        let sessions = open_configured_root(&app_paths::app_sessions_root())?;
        let session = open_dir_at(&sessions, session_id)?;
        open_dir_at(&session, "artifacts")
    }

    pub fn open_dir_chain(root: &File, components: &[&str]) -> io::Result<File> {
        let mut current = root.try_clone()?;
        for component in components {
            current = open_dir_at(&current, component)?;
        }
        Ok(current)
    }

    pub fn entry_names(dir: &File) -> io::Result<Vec<Vec<u8>>> {
        // dup() shares the directory offset with `dir`; multiple scans through
        // one trusted handle would make every scan after the first appear
        // empty. openat(".") gives fdopendir an independent open description.
        let descriptor = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let pointer = unsafe { libc::fdopendir(descriptor) };
        if pointer.is_null() {
            unsafe {
                libc::close(descriptor);
            }
            return Err(io::Error::last_os_error());
        }
        let stream = DirStream(pointer);
        let mut names = Vec::new();
        loop {
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            names.push(name.to_vec());
        }
        Ok(names)
    }

    pub fn metadata_at(dir: &File, name: &[u8]) -> io::Result<Metadata> {
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL file name"))?;
        let mut metadata = MaybeUninit::<libc::stat>::zeroed();
        let result = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        let metadata = unsafe { metadata.assume_init() };
        let seconds = metadata.st_mtime;
        let nanos = metadata.st_mtime_nsec;
        let modified_at_unix_ms = if seconds < 0 {
            0
        } else {
            (seconds as u64)
                .saturating_mul(1_000)
                .saturating_add((nanos.max(0) as u64) / 1_000_000)
        };
        Ok(Metadata {
            regular_file: metadata.st_mode & libc::S_IFMT == libc::S_IFREG,
            size: metadata.st_size.max(0) as u64,
            modified_at_unix_ms,
        })
    }

    fn open_regular_at(
        dir: &File,
        name: &[u8],
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<File> {
        let name = safe_leaf(name)?;
        let descriptor = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        let file_type = file.metadata()?.file_type();
        if !file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "storage entry is not a regular file",
            ));
        }
        Ok(file)
    }

    pub fn open_regular_read_at(dir: &File, name: &[u8]) -> io::Result<File> {
        open_regular_at(dir, name, libc::O_RDONLY, 0)
    }

    pub fn open_regular_rw_create_at(dir: &File, name: &[u8]) -> io::Result<File> {
        // macOS can return a transient ENOENT when two openat(O_CREAT) calls
        // race to create the same previously absent leaf. Make ownership of
        // creation explicit, then open the winner without O_CREAT. Retrying
        // only the create/open handoff also covers an independently removed
        // leaf without weakening O_NOFOLLOW.
        for _ in 0..4 {
            match open_regular_at(
                dir,
                name,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            ) {
                Ok(file) => return Ok(file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    match open_regular_at(dir, name, libc::O_RDWR, 0) {
                        Ok(file) => return Ok(file),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "regular file changed repeatedly during create/open",
        ))
    }

    pub fn read_regular_at(dir: &File, name: &[u8], max_bytes: u64) -> io::Result<Vec<u8>> {
        let file = open_regular_read_at(dir, name)?;
        let size = file.metadata()?.len();
        if size > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "regular file exceeds read limit",
            ));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "regular file exceeds read limit",
            ));
        }
        Ok(bytes)
    }

    pub fn open_and_lock_file(dir: &File, name: &[u8]) -> io::Result<File> {
        let file = open_regular_rw_create_at(dir, name)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }

    pub fn atomic_write_regular_at(dir: &File, name: &[u8], bytes: &[u8]) -> io::Result<()> {
        match metadata_at(dir, name) {
            Ok(metadata) if !metadata.regular_file => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "atomic-write target is not a regular file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let temp_name = format!(".unpeel-upload-tmp-{}", uuid::Uuid::new_v4());
        let temp_encoded = safe_leaf(temp_name.as_bytes())?;
        let descriptor = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                temp_encoded.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut temp = unsafe { File::from_raw_fd(descriptor) };
        let write_result = (|| {
            temp.write_all(bytes)?;
            temp.sync_all()?;
            Ok::<(), io::Error>(())
        })();
        if let Err(error) = write_result {
            let _ = unsafe { libc::unlinkat(dir.as_raw_fd(), temp_encoded.as_ptr(), 0) };
            return Err(error);
        }
        let destination = safe_leaf(name)?;
        let result = unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                temp_encoded.as_ptr(),
                dir.as_raw_fd(),
                destination.as_ptr(),
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            let _ = unsafe { libc::unlinkat(dir.as_raw_fd(), temp_encoded.as_ptr(), 0) };
            return Err(error);
        }
        sync_directory(dir)
    }

    pub fn link_regular_at(dir: &File, source: &[u8], destination: &[u8]) -> io::Result<()> {
        let metadata = metadata_at(dir, source)?;
        if !metadata.regular_file {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "publish source is not a regular file",
            ));
        }
        let source = safe_leaf(source)?;
        let destination = safe_leaf(destination)?;
        let result = unsafe {
            libc::linkat(
                dir.as_raw_fd(),
                source.as_ptr(),
                dir.as_raw_fd(),
                destination.as_ptr(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn rename_regular_at(dir: &File, source: &[u8], destination: &[u8]) -> io::Result<()> {
        if !metadata_at(dir, source)?.regular_file {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rename source is not a regular file",
            ));
        }
        match metadata_at(dir, destination) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "rename destination already exists",
                ))
            }
            Err(error) => return Err(error),
        }
        let source = safe_leaf(source)?;
        let destination = safe_leaf(destination)?;
        let result = unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                source.as_ptr(),
                dir.as_raw_fd(),
                destination.as_ptr(),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        sync_directory(dir)
    }

    pub fn unlink_regular_at(dir: &File, name: &[u8]) -> io::Result<()> {
        let metadata = match metadata_at(dir, name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.regular_file {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unlink target is not a regular file",
            ));
        }
        let name = safe_leaf(name)?;
        let result = unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        }
    }

    pub fn sync_directory(dir: &File) -> io::Result<()> {
        let result = unsafe { libc::fsync(dir.as_raw_fd()) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn unlink_regular_or_symlink(dir: &File, name: &[u8]) -> io::Result<()> {
        let metadata = match metadata_at(dir, name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.regular_file {
            let name = CString::new(name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL file name"))?;
            let mut raw = MaybeUninit::<libc::stat>::zeroed();
            let result = unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    raw.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result != 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            let raw = unsafe { raw.assume_init() };
            if raw.st_mode & libc::S_IFMT != libc::S_IFLNK {
                return Ok(());
            }
        }
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL file name"))?;
        let result = unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[cfg(test)]
    pub fn open_root_for_test(path: &std::path::Path) -> io::Result<File> {
        open_dir(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        std::fs::canonicalize(std::env::temp_dir())
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(format!("unpeel-artifacts-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[cfg(unix)]
    fn create_test_session(sessions_root: &Path, session_id: &str) -> PathBuf {
        let session = sessions_root.join(session_id);
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "session": { "id": session_id }
            }))
            .unwrap(),
        )
        .unwrap();
        session
    }

    #[cfg(unix)]
    fn png_bytes() -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(b"unpeel resumable image payload");
        bytes
    }

    #[cfg(unix)]
    #[test]
    fn local_image_publication_adds_a_verified_gallery_upload() {
        let root = temp_root("local-publish");
        let session_id = "session-local-publish";
        let session = create_test_session(&root, session_id);
        let source = root.join("Visual result.png");
        let image = png_bytes();
        std::fs::write(&source, &image).unwrap();

        let published = publish_local_image_at(&root, session_id, &source).unwrap();
        assert_eq!(published.content_type, "image/png");
        assert_eq!(published.size, image.len() as u64);
        assert!(published.name.starts_with("Visual-result-"));
        assert!(published.name.ends_with(".png"));
        assert_eq!(std::fs::read(&published.path).unwrap(), image);
        assert_eq!(
            published.path.parent().unwrap(),
            session.join("artifacts/uploads")
        );

        let listed = list_at(&session.join("artifacts"));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, "uploads");
        assert_eq!(listed[0].name, published.name);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn local_image_publication_rejects_a_mismatched_signature() {
        let root = temp_root("local-publish-signature");
        let session_id = "session-local-publish-signature";
        let session = create_test_session(&root, session_id);
        let source = root.join("not-an-image.png");
        std::fs::write(&source, b"plain text").unwrap();

        let error = publish_local_image_at(&root, session_id, &source).unwrap_err();
        assert!(error.contains("do not match"));
        assert!(!session.join("artifacts").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    fn send_chunk(
        sessions_root: &Path,
        session_id: &str,
        upload_id: &str,
        offset: u64,
        complete_bytes: &[u8],
        chunk: &[u8],
        content_type: &str,
        principal: &str,
    ) -> Result<ResumableArtifactUploadResult, ResumableArtifactUploadError> {
        let digest = sha256_hex(complete_bytes);
        send_raw_chunk(
            sessions_root,
            session_id,
            upload_id,
            offset,
            complete_bytes.len() as u64,
            &digest,
            chunk,
            content_type,
            principal,
        )
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    fn send_raw_chunk(
        sessions_root: &Path,
        session_id: &str,
        upload_id: &str,
        offset: u64,
        total_size: u64,
        digest: &str,
        chunk: &[u8],
        content_type: &str,
        principal: &str,
    ) -> Result<ResumableArtifactUploadResult, ResumableArtifactUploadError> {
        upload_resumable_artifact_chunk_at(
            sessions_root,
            ResumableArtifactUploadRequest {
                session_id,
                upload_id,
                offset,
                total_size,
                sha256: digest,
                content_type,
                principal,
                bytes: chunk,
            },
        )
    }

    #[test]
    fn list_skips_hidden_directories_and_symlinks() {
        let root = temp_root("list");
        let screenshots = kind_dir_at(&root, "screenshots").unwrap();
        std::fs::create_dir_all(&screenshots).unwrap();
        std::fs::write(screenshots.join("result.png"), b"png").unwrap();
        std::fs::write(screenshots.join(".partial"), b"hidden").unwrap();
        std::fs::create_dir(screenshots.join("folder.png")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("result.png", screenshots.join("link.png")).unwrap();

        let listed = list_at(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, "screenshots");
        assert_eq!(listed[0].name, "result.png");
        assert_eq!(listed[0].size, 3);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_chunk_pages_original_bytes_and_clamps_ranges() {
        let root = temp_root("read-chunk");
        let screenshots = kind_dir_at(&root, "screenshots").unwrap();
        std::fs::create_dir_all(&screenshots).unwrap();
        std::fs::write(screenshots.join("result.PNG"), b"0123456789").unwrap();

        let components = kind_components("screenshots").unwrap();
        let first = read_chunk_at(&root, components, "screenshots", "result.PNG", 3, 4)
            .expect("first artifact range");
        assert_eq!(first.content_type, "image/png");
        assert_eq!(first.offset, 3);
        assert_eq!(first.next_offset, 7);
        assert_eq!(first.total_size, 10);
        assert_eq!(first.bytes, b"3456");

        let end = read_chunk_at(&root, components, "screenshots", "result.PNG", u64::MAX, 0)
            .expect("range beyond eof");
        assert_eq!(end.offset, 10);
        assert_eq!(end.next_offset, 10);
        assert!(end.bytes.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn read_chunk_never_follows_leaf_or_kind_symlinks() {
        let root = temp_root("read-symlink");
        let outside = temp_root("read-symlink-outside");
        std::fs::create_dir_all(kind_dir_at(&root, "screenshots").unwrap()).unwrap();
        std::fs::create_dir_all(outside.join("screenshots")).unwrap();
        std::fs::write(outside.join("screenshots/private.png"), b"private").unwrap();
        std::os::unix::fs::symlink(
            outside.join("screenshots/private.png"),
            kind_dir_at(&root, "screenshots").unwrap().join("leaf.png"),
        )
        .unwrap();

        let components = kind_components("screenshots").unwrap();
        let leaf = read_chunk_at(&root, components, "screenshots", "leaf.png", 0, 100)
            .expect_err("leaf symlink must be rejected");
        assert_eq!(leaf.status, 404);

        std::fs::remove_dir_all(root.join("browser")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("browser")).unwrap();
        let ancestor = read_chunk_at(&root, components, "screenshots", "private.png", 0, 100)
            .expect_err("kind symlink must be rejected");
        assert_eq!(ancestor.status, 404);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn delete_is_idempotent_and_reaps_thumbnail_variants() {
        let root = temp_root("delete");
        let screenshots = kind_dir_at(&root, "screenshots").unwrap();
        let thumbs = root.join("thumbs");
        std::fs::create_dir_all(&screenshots).unwrap();
        std::fs::create_dir_all(&thumbs).unwrap();
        std::fs::write(screenshots.join("result.png"), b"png").unwrap();
        std::fs::write(thumbs.join("1-256-screenshots-result.png.jpg"), b"jpg").unwrap();
        std::fs::write(thumbs.join("keep.jpg"), b"jpg").unwrap();

        delete_at(&root, "screenshots", "result.png").unwrap();
        delete_at(&root, "screenshots", "result.png").unwrap();
        assert!(!screenshots.join("result.png").exists());
        assert!(!thumbs.join("1-256-screenshots-result.png.jpg").exists());
        assert!(thumbs.join("keep.jpg").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn list_and_delete_never_follow_symlinked_kind_directories() {
        let root = temp_root("ancestor-symlink");
        let outside = temp_root("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(outside.join("screenshots")).unwrap();
        std::fs::write(outside.join("screenshots/victim.png"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("browser")).unwrap();

        assert!(list_at(&root).is_empty());
        assert!(delete_at(&root, "screenshots", "victim.png").is_err());
        assert_eq!(
            std::fs::read(outside.join("screenshots/victim.png")).unwrap(),
            b"private"
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_publishes_exact_bytes_and_hides_internal_state() {
        let root = temp_root("resumable-publish");
        let session_id = "session-one";
        let session = create_test_session(&root, session_id);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let image = png_bytes();
        let split = 13_usize;

        let first = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert_eq!(first.next_offset, split as u64);
        assert!(!first.complete);
        assert!(first.path.is_none());
        assert!(first.name.is_none());

        // A transport retry of an already durable full range is a no-op.
        let duplicate = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert_eq!(duplicate, first);

        let completed = send_chunk(
            &root,
            session_id,
            &upload_id,
            split as u64,
            &image,
            &image[split..],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert!(completed.complete);
        assert_eq!(completed.next_offset, image.len() as u64);
        assert_eq!(completed.content_type.as_deref(), Some("image/png"));
        assert_eq!(
            completed.sha256.as_deref(),
            Some(sha256_hex(&image).as_str())
        );
        let published = completed.path.as_ref().unwrap();
        assert_eq!(std::fs::read(published).unwrap(), image);
        assert_eq!(
            published.file_name().unwrap(),
            completed.name.as_deref().unwrap()
        );
        assert!(completed.name.as_deref().unwrap().starts_with("upload-"));

        let listed = list_at(&session.join("artifacts"));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, "uploads");
        assert_eq!(listed[0].name, completed.name.unwrap());
        assert_eq!(listed[0].size, image.len() as u64);

        // Retrying any exact published range survives process/app restarts;
        // there is deliberately no in-memory upload registry involved.
        let after_restart = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert!(after_restart.complete);
        assert_eq!(after_restart.path.as_ref(), Some(published));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_accepts_exact_chunk_and_total_size_ceilings() {
        let root = temp_root("resumable-exact-ceilings");
        let session_id = "session-exact-ceilings";
        create_test_session(&root, session_id);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let mut image = vec![0x5a; RESUMABLE_UPLOAD_MAX_TOTAL_BYTES as usize];
        image[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let digest = sha256_hex(&image);
        let chunk_count = image.len() / RESUMABLE_UPLOAD_MAX_CHUNK_BYTES;
        assert_eq!(chunk_count, 16);

        let mut final_result = None;
        for index in 0..chunk_count {
            let start = index * RESUMABLE_UPLOAD_MAX_CHUNK_BYTES;
            let end = start + RESUMABLE_UPLOAD_MAX_CHUNK_BYTES;
            let result = send_raw_chunk(
                &root,
                session_id,
                &upload_id,
                start as u64,
                RESUMABLE_UPLOAD_MAX_TOTAL_BYTES,
                &digest,
                &image[start..end],
                "image/png",
                "device:alice",
            )
            .unwrap();
            assert_eq!(result.next_offset, end as u64);
            assert_eq!(result.complete, index + 1 == chunk_count);
            if result.complete {
                final_result = Some(result);
            }
        }
        let completed = final_result.expect("the sixteenth exact-size chunk publishes");
        assert_eq!(
            std::fs::read(completed.path.unwrap()).unwrap(),
            image,
            "the 4 MiB publication must preserve every byte"
        );

        let too_large = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            RESUMABLE_UPLOAD_MAX_TOTAL_BYTES + 1,
            &digest,
            b"x",
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(too_large.status, 413);
        assert_eq!(too_large.code, "upload_too_large");

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_identical_initial_chunks_serialize_idempotently() {
        use std::sync::{Arc, Barrier};

        let root = temp_root("resumable-concurrent-initial");
        let session_id = "session-concurrent-initial";
        let session = create_test_session(&root, session_id);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let image = png_bytes();
        let split = 12_usize;
        let barrier = Arc::new(Barrier::new(3));

        let mut workers = Vec::new();
        for _ in 0..2 {
            let worker_root = root.clone();
            let worker_upload_id = upload_id.clone();
            let worker_image = image.clone();
            let worker_barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                worker_barrier.wait();
                send_chunk(
                    &worker_root,
                    session_id,
                    &worker_upload_id,
                    0,
                    &worker_image,
                    &worker_image[..split],
                    "image/png",
                    "device:alice",
                )
            }));
        }
        barrier.wait();
        let first = workers.remove(0).join().unwrap().unwrap();
        let second = workers.remove(0).join().unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.next_offset, split as u64);
        assert!(!first.complete);

        let key = sha256_hex(upload_id.as_bytes());
        let uploads = session.join("artifacts/uploads");
        let part = uploads.join(format!(".unpeel-upload-{key}.part"));
        assert_eq!(std::fs::read(part).unwrap(), image[..split]);
        let state: ResumableUploadState = serde_json::from_slice(
            &std::fs::read(uploads.join(format!(".unpeel-upload-{key}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(state.committed_offset, split as u64);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_recovers_publish_before_receipt_commit() {
        let root = temp_root("resumable-recover");
        let session_id = "session-recover";
        let session = create_test_session(&root, session_id);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let image = png_bytes();
        let completed = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap();
        let uploads = session.join("artifacts/uploads");
        let key = sha256_hex(upload_id.as_bytes());
        let state_path = uploads.join(format!(".unpeel-upload-{key}.json"));
        let part_path = uploads.join(format!(".unpeel-upload-{key}.part"));
        let mut state: ResumableUploadState =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        state.complete = false;
        std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        std::fs::hard_link(completed.path.as_ref().unwrap(), &part_path).unwrap();

        let recovered = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert!(recovered.complete);
        assert_eq!(recovered.path, completed.path);
        assert!(!part_path.exists());
        let recovered_state: ResumableUploadState =
            serde_json::from_slice(&std::fs::read(state_path).unwrap()).unwrap();
        assert!(recovered_state.complete);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_discards_uncommitted_tail_after_process_crash() {
        let root = temp_root("resumable-uncommitted-tail");
        let session_id = "session-uncommitted-tail";
        let session = create_test_session(&root, session_id);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let image = png_bytes();
        let split = 11_usize;
        send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();

        // Simulate write_all reaching the staging inode before the process
        // could atomically advance committed_offset in its receipt metadata.
        let key = sha256_hex(upload_id.as_bytes());
        let part_path = session
            .join("artifacts/uploads")
            .join(format!(".unpeel-upload-{key}.part"));
        use std::io::Write as _;
        let mut part = std::fs::OpenOptions::new()
            .append(true)
            .open(&part_path)
            .unwrap();
        part.write_all(b"uncommitted garbage").unwrap();
        part.sync_all().unwrap();
        drop(part);

        let completed = send_chunk(
            &root,
            session_id,
            &upload_id,
            split as u64,
            &image,
            &image[split..],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert!(completed.complete);
        assert_eq!(std::fs::read(completed.path.unwrap()).unwrap(), image);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_expires_only_stale_incomplete_state() {
        let root = temp_root("resumable-expiry");
        let session_id = "session-expiry";
        let session = create_test_session(&root, session_id);
        let uploads = session.join("artifacts/uploads");
        let image = png_bytes();
        let split = 10_usize;

        let stale_id = uuid::Uuid::new_v4().to_string();
        let fresh_id = uuid::Uuid::new_v4().to_string();
        for upload_id in [&stale_id, &fresh_id] {
            send_chunk(
                &root,
                session_id,
                upload_id,
                0,
                &image,
                &image[..split],
                "image/png",
                "device:alice",
            )
            .unwrap();
        }

        let completed_id = uuid::Uuid::new_v4().to_string();
        let completed = send_chunk(
            &root,
            session_id,
            &completed_id,
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap();
        let failed_id = uuid::Uuid::new_v4().to_string();
        let invalid = b"not a png image";
        let failed = send_raw_chunk(
            &root,
            session_id,
            &failed_id,
            0,
            invalid.len() as u64,
            &sha256_hex(invalid),
            invalid,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(failed.status, 415);

        let state_path = |upload_id: &str| {
            uploads.join(format!(
                ".unpeel-upload-{}.json",
                sha256_hex(upload_id.as_bytes())
            ))
        };
        let part_path = |upload_id: &str| {
            uploads.join(format!(
                ".unpeel-upload-{}.part",
                sha256_hex(upload_id.as_bytes())
            ))
        };
        let stale_timestamp = upload_now_unix_ms()
            .saturating_sub(RESUMABLE_UPLOAD_INCOMPLETE_TTL_MS.saturating_add(1));
        for upload_id in [&stale_id, &completed_id, &failed_id] {
            let path = state_path(upload_id);
            let mut state: ResumableUploadState =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            state.updated_at_unix_ms = stale_timestamp;
            std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        }

        // Any subsequent authorized upload performs maintenance under the
        // same Session lock before making its quota decision.
        let newcomer_id = uuid::Uuid::new_v4().to_string();
        send_chunk(
            &root,
            session_id,
            &newcomer_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();

        assert!(!state_path(&stale_id).exists());
        assert!(!part_path(&stale_id).exists());
        assert!(state_path(&fresh_id).exists());
        assert_eq!(std::fs::read(part_path(&fresh_id)).unwrap(), image[..split]);
        assert!(state_path(&completed_id).exists());
        assert_eq!(std::fs::read(completed.path.unwrap()).unwrap(), image);
        assert!(state_path(&failed_id).exists());

        // Expiry intentionally resets resume to offset zero; the old bytes and
        // principal binding are gone only after the 24-hour inactivity bound.
        let expired_resume = send_chunk(
            &root,
            session_id,
            &stale_id,
            split as u64,
            &image,
            &image[split..split + 1],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(expired_resume.status, 409);
        assert_eq!(expired_resume.next_offset, Some(0));
        let restarted = send_chunk(
            &root,
            session_id,
            &stale_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert_eq!(restarted.next_offset, split as u64);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_recovers_crash_during_expiry_cleanup() {
        let root = temp_root("resumable-expiry-crash");
        let session_id = "session-expiry-crash";
        let session = create_test_session(&root, session_id);
        let image = png_bytes();
        let upload_id = uuid::Uuid::new_v4().to_string();
        send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..10],
            "image/png",
            "device:alice",
        )
        .unwrap();
        let uploads = session.join("artifacts/uploads");
        let key = sha256_hex(upload_id.as_bytes());
        let state = uploads.join(format!(".unpeel-upload-{key}.json"));
        let expired = uploads.join(format!(".unpeel-upload-{key}.expired"));
        let part = uploads.join(format!(".unpeel-upload-{key}.part"));

        // The atomic receipt rename happened, then the Host died before it
        // could unlink either the part or tombstone.
        std::fs::rename(&state, &expired).unwrap();
        assert!(part.exists());
        let next_id = uuid::Uuid::new_v4().to_string();
        send_chunk(
            &root,
            session_id,
            &next_id,
            0,
            &image,
            &image[..10],
            "image/png",
            "device:alice",
        )
        .unwrap();
        assert!(!expired.exists());
        assert!(!part.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn storage_errors_preserve_raw_errno_for_507_mapping() {
        for raw_code in [libc::ENOSPC, libc::EDQUOT] {
            let mapped = upload_storage_error(
                "persist upload",
                std::io::Error::from_raw_os_error(raw_code),
            );
            assert_eq!(mapped.status, 507);
            assert_eq!(mapped.code, "insufficient_storage");
        }

        let root = temp_root("raw-storage-error");
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("lock")).unwrap();
        let opened = secure_fs::open_root_for_test(&root).unwrap();
        let error = secure_fs::open_and_lock_file(&opened, b"lock").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_rejects_gaps_overlaps_conflicts_and_id_reuse() {
        let root = temp_root("resumable-conflicts");
        let session_id = "session-conflicts";
        create_test_session(&root, session_id);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let image = png_bytes();
        let split = 12_usize;
        send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:alice",
        )
        .unwrap();

        let gap = send_chunk(
            &root,
            session_id,
            &upload_id,
            split as u64 + 1,
            &image,
            &image[split + 1..split + 3],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(gap.status, 409);
        assert_eq!(gap.next_offset, Some(split as u64));

        let overlap = send_chunk(
            &root,
            session_id,
            &upload_id,
            split as u64 - 1,
            &image,
            &image[split - 1..split + 1],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(overlap.status, 409);
        assert_eq!(overlap.next_offset, Some(split as u64));

        let mut different = image[..split].to_vec();
        different[0] ^= 1;
        let conflict = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &different,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.next_offset, Some(split as u64));

        let principal_reuse = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/png",
            "device:bob",
        )
        .unwrap_err();
        assert_eq!(principal_reuse.status, 409);

        let mime_reuse = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image[..split],
            "image/jpeg",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(mime_reuse.status, 409);

        let changed_total = image[..image.len() - 1].to_vec();
        let metadata_reuse = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &changed_total,
            &changed_total[..split],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(metadata_reuse.status, 409);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_enforces_request_bounds_and_quota() {
        let root = temp_root("resumable-bounds");
        let session_id = "session-bounds";
        create_test_session(&root, session_id);
        let image = png_bytes();
        let digest = sha256_hex(&image);

        let too_large_chunk = vec![0_u8; RESUMABLE_UPLOAD_MAX_CHUNK_BYTES + 1];
        let error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            too_large_chunk.len() as u64,
            &sha256_hex(&too_large_chunk),
            &too_large_chunk,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 413);
        assert_eq!(error.code, "chunk_too_large");

        let error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            RESUMABLE_UPLOAD_MAX_TOTAL_BYTES + 1,
            &digest,
            &image[..1],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 413);

        let error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            image.len() as u64,
            image.len() as u64,
            &digest,
            &image[..1],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(error.code, "invalid_range");

        let error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            image.len() as u64,
            &digest,
            &[],
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.code, "empty_chunk");

        let upper_id = uuid::Uuid::new_v4().to_string().to_uppercase();
        let error = send_raw_chunk(
            &root,
            session_id,
            &upper_id,
            0,
            image.len() as u64,
            &digest,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_upload_id");

        let error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            image.len() as u64,
            &digest.to_uppercase(),
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_sha256");

        let error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            image.len() as u64,
            &digest,
            &image,
            "image/gif",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 415);

        let new_gap_id = uuid::Uuid::new_v4().to_string();
        let new_gap = send_raw_chunk(
            &root,
            session_id,
            &new_gap_id,
            1,
            9,
            &"0".repeat(64),
            b"x",
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(new_gap.status, 409);
        assert_eq!(new_gap.next_offset, Some(0));
        let gap_key = sha256_hex(new_gap_id.as_bytes());
        assert!(!root
            .join(session_id)
            .join("artifacts/uploads")
            .join(format!(".unpeel-upload-{gap_key}.json"))
            .exists());

        for _ in 0..RESUMABLE_UPLOAD_MAX_ACTIVE {
            let id = uuid::Uuid::new_v4().to_string();
            send_raw_chunk(
                &root,
                session_id,
                &id,
                0,
                9,
                &"0".repeat(64),
                b"x",
                "image/png",
                "device:alice",
            )
            .unwrap();
        }
        let quota_error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            9,
            &"0".repeat(64),
            b"x",
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(quota_error.status, 429);
        assert_eq!(quota_error.code, "upload_quota_exceeded");

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_verifies_digest_and_image_magic_before_publish() {
        let root = temp_root("resumable-integrity");
        let session_id = "session-integrity";
        let session = create_test_session(&root, session_id);
        let image = png_bytes();

        let digest_error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            image.len() as u64,
            &"0".repeat(64),
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(digest_error.status, 422);
        assert_eq!(digest_error.code, "upload_digest_mismatch");

        let not_an_image = b"this is definitely not a png";
        let signature_error = send_raw_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            not_an_image.len() as u64,
            &sha256_hex(not_an_image),
            not_an_image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(signature_error.status, 415);
        assert_eq!(signature_error.code, "unsupported_media_type");
        let uploads = session.join("artifacts/uploads");
        assert!(std::fs::read_dir(uploads)
            .unwrap()
            .flatten()
            .all(|entry| entry.file_name().to_string_lossy().starts_with('.')));

        // Terminal integrity failures leave a small binding receipt but no
        // staging bytes and do not consume one of the active-upload slots.
        for _ in 0..RESUMABLE_UPLOAD_MAX_ACTIVE {
            send_raw_chunk(
                &root,
                session_id,
                &uuid::Uuid::new_v4().to_string(),
                0,
                9,
                &"0".repeat(64),
                b"x",
                "image/png",
                "device:alice",
            )
            .unwrap();
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_refuses_symlinked_roots_and_session_ancestors() {
        let actual_root = temp_root("actual-root");
        let root_link = temp_root("root-link");
        let session_id = "session-symlink-root";
        let actual_session = create_test_session(&actual_root, session_id);
        std::os::unix::fs::symlink(&actual_root, &root_link).unwrap();
        let image = png_bytes();
        let error = send_chunk(
            &root_link,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 404);
        assert!(!actual_session.join("artifacts").exists());

        let sessions_root = temp_root("session-link-root");
        std::fs::create_dir_all(&sessions_root).unwrap();
        let outside = temp_root("session-link-outside");
        let outside_session = create_test_session(&outside, session_id);
        std::os::unix::fs::symlink(&outside_session, sessions_root.join(session_id)).unwrap();
        let error = send_chunk(
            &sessions_root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 404);
        assert!(!outside_session.join("artifacts").exists());

        let manifest_root = temp_root("manifest-link-root");
        let manifest_session = manifest_root.join(session_id);
        std::fs::create_dir_all(&manifest_session).unwrap();
        let outside_manifest = manifest_root.join("outside-manifest.json");
        std::fs::write(
            &outside_manifest,
            format!(r#"{{"session":{{"id":"{session_id}"}}}}"#),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_manifest, manifest_session.join("manifest.json"))
            .unwrap();
        let error = send_chunk(
            &manifest_root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 404);
        assert!(!manifest_session.join("artifacts").exists());

        let _ = std::fs::remove_file(root_link);
        let _ = std::fs::remove_dir_all(actual_root);
        let _ = std::fs::remove_dir_all(sessions_root);
        let _ = std::fs::remove_dir_all(outside);
        let _ = std::fs::remove_dir_all(manifest_root);
    }

    #[cfg(unix)]
    #[test]
    fn resumable_upload_refuses_symlinked_writable_directories_and_state() {
        let root = temp_root("writable-symlinks");
        let session_id = "session-writable-symlinks";
        let session = create_test_session(&root, session_id);
        let outside = temp_root("writable-symlinks-outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, session.join("artifacts")).unwrap();
        let image = png_bytes();
        let error = send_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 500);
        assert!(!outside.join("uploads").exists());
        std::fs::remove_file(session.join("artifacts")).unwrap();

        let uploads_outside = temp_root("uploads-symlink-outside");
        std::fs::create_dir_all(&uploads_outside).unwrap();
        std::fs::create_dir(session.join("artifacts")).unwrap();
        std::os::unix::fs::symlink(&uploads_outside, session.join("artifacts/uploads")).unwrap();
        let error = send_chunk(
            &root,
            session_id,
            &uuid::Uuid::new_v4().to_string(),
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 500);
        assert!(std::fs::read_dir(&uploads_outside)
            .unwrap()
            .next()
            .is_none());
        std::fs::remove_file(session.join("artifacts/uploads")).unwrap();
        std::fs::create_dir(session.join("artifacts/uploads")).unwrap();

        let upload_id = uuid::Uuid::new_v4().to_string();
        let key = sha256_hex(upload_id.as_bytes());
        let outside_file = outside.join("outside-state");
        std::fs::write(&outside_file, b"untouched").unwrap();
        let lock_path = session
            .join("artifacts/uploads")
            .join(".unpeel-upload.lock");
        std::os::unix::fs::symlink(&outside_file, &lock_path).unwrap();
        let error = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 500);
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"untouched");

        std::fs::remove_file(lock_path).unwrap();
        let part_path = session
            .join("artifacts/uploads")
            .join(format!(".unpeel-upload-{key}.part"));
        std::os::unix::fs::symlink(&outside_file, &part_path).unwrap();
        let error = send_chunk(
            &root,
            session_id,
            &upload_id,
            0,
            &image,
            &image,
            "image/png",
            "device:alice",
        )
        .unwrap_err();
        assert_eq!(error.status, 409);
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"untouched");

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
        let _ = std::fs::remove_dir_all(uploads_outside);
    }
}
