//! Shared-secret auth for the `/mcp/*` lifecycle routes on the app's hook
//! server port. The endpoints can launch arbitrary commands, so unlike the
//! hook routes they require this header. The token lives in a user-only file
//! that the MCP host (same user) reads; a malicious webpage doing
//! cross-origin POSTs to localhost cannot read it.

use crate::app_paths::unpeel_home;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MCP_AUTH_HEADER: &str = "x-unpeel-auth";

pub fn auth_token_path() -> PathBuf {
    unpeel_home().join("mcp").join("auth-token")
}

fn token_cache() -> &'static Mutex<Option<String>> {
    static CACHE: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Read the shared MCP auth token, creating it on first use. The token is
/// shared across Unpeel instances (they all trust the same user) and cached
/// per process so concurrent callers cannot race the file into regeneration.
pub fn ensure_auth_token() -> Result<String, String> {
    let mut cache = token_cache().lock().unwrap();
    if let Some(token) = cache.as_ref() {
        return Ok(token.clone());
    }

    let path = auth_token_path();
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            *cache = Some(trimmed.to_string());
            return Ok(trimmed.to_string());
        }
    }

    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create MCP dir {}: {e}", parent.display()))?;
    }
    fs::write(&path, format!("{token}\n"))
        .map_err(|e| format!("Failed to write MCP auth token: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    *cache = Some(token.clone());
    Ok(token)
}

pub fn verify_auth(provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let Ok(expected) = ensure_auth_token() else {
        return false;
    };
    // Same-length comparison; the token is high-entropy and local-only, so a
    // simple equality check is sufficient.
    provided.trim() == expected
}
