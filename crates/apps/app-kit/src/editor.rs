//! Shared “Open in editor” bridge for standalone and Unpeel-hosted Apps.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::json;

/// Failure to resolve or open a requested filesystem item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorError(String);

impl EditorError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for EditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EditorError {}

/// Opens files and folders in Unpeel's configured editor when hosted, with a
/// platform opener fallback for the same App running standalone.
pub struct EditorBridge;

impl EditorBridge {
    pub fn open(path: impl AsRef<Path>) -> Result<(), EditorError> {
        open_in_editor(path)
    }
}

/// Open a filesystem item in the user's editor.
///
/// Hosted Apps ask the owning local Unpeel instance first, which preserves
/// its Settings ▸ General editor choice. If no compatible desktop instance is
/// reachable, the platform's ordinary opener is used instead.
pub fn open_in_editor(path: impl AsRef<Path>) -> Result<(), EditorError> {
    let path = absolute_existing_path(path.as_ref())?;
    if try_unpeel_editor(&path) {
        return Ok(());
    }
    open_with_platform(&path)
}

fn absolute_existing_path(path: &Path) -> Result<PathBuf, EditorError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| EditorError::new(format!("cannot resolve current folder: {error}")))?
            .join(path)
    };
    std::fs::canonicalize(&absolute)
        .map_err(|error| EditorError::new(format!("cannot open {}: {error}", absolute.display())))
}

fn try_unpeel_editor(path: &Path) -> bool {
    let Some(port) = std::env::var("UNPEEL_APP_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
    else {
        return false;
    };
    let Ok(session_id) = std::env::var("UNPEEL_SESSION_ID") else {
        return false;
    };
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return false;
    }
    post_open_request(port, &session_id, path).unwrap_or(false)
}

fn post_open_request(port: u16, session_id: &str, path: &Path) -> io::Result<bool> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let body = serde_json::to_vec(&json!({ "path": path }))?;
    write!(
        stream,
        "POST /open-in-editor/{session_id} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.shutdown(Shutdown::Write)?;

    let mut response = String::new();
    stream.take(8 * 1024).read_to_string(&mut response)?;
    Ok(response
        .lines()
        .next()
        .is_some_and(|line| line.split_whitespace().nth(1) == Some("200")))
}

fn open_with_platform(path: &Path) -> Result<(), EditorError> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(EditorError::new(
        "Unpeel is unavailable and this platform has no configured file opener",
    ));

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        command
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                EditorError::new(format!("could not open {}: {error}", path.display()))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn missing_paths_fail_before_opening_any_editor() {
        let missing = tempfile::tempdir().unwrap().path().join("missing.txt");
        assert!(
            open_in_editor(&missing)
                .unwrap_err()
                .to_string()
                .contains("cannot open")
        );
    }

    #[test]
    fn hosted_request_posts_the_absolute_path_to_the_local_owner() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a note.md");
        std::fs::write(&file, "hello").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        let expected = canonical.to_string_lossy().into_owned();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            assert!(request.starts_with("POST /open-in-editor/session-1 HTTP/1.1\r\n"));
            assert!(request.contains(&expected));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
                .unwrap();
        });

        assert!(post_open_request(port, "session-1", &canonical).unwrap());
        server.join().unwrap();
    }
}
