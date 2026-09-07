//! Crash-safe state convention for always-on hosted Apps.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::MAX_SAFE_UI_INTEGER;

/// Stable filename stored next to the Host-owned `ui.sock`.
pub const UI_STATE_FILENAME: &str = "ui-state.json";
/// Envelope discriminator for persisted App state.
pub const UI_STATE_FORMAT: &str = "unpeel.app-kit.state";
/// Current persistence-envelope version. App model schemas version separately.
pub const UI_STATE_FORMAT_VERSION: u32 = 1;

const MAX_UI_STATE_BYTES: u64 = 64 * 1024 * 1024;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

/// Versioned state envelope. Apps own `state` and its schema migrations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSavedState<T> {
    pub format: String,
    pub format_version: u32,
    pub app_id: String,
    pub app_version: String,
    pub state_schema_version: u32,
    pub revision: u64,
    pub saved_at_unix_ms: u64,
    pub state: T,
}

/// One App's durable state location beneath its Host session directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiStateStore {
    path: PathBuf,
    app_id: String,
    app_version: String,
}

impl UiStateStore {
    /// Creates a store at an explicit file. Its parent directory must exist.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        app_id: impl Into<String>,
        app_version: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            app_id: app_id.into(),
            app_version: app_version.into(),
        }
    }

    pub(crate) fn beside_socket(
        socket_path: &Path,
        app_id: impl Into<String>,
        app_version: impl Into<String>,
    ) -> Result<Self, UiStateError> {
        let parent = socket_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| UiStateError::InvalidPath(socket_path.to_owned()))?;
        Ok(Self::new(
            parent.join(UI_STATE_FILENAME),
            app_id,
            app_version,
        ))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads a complete envelope. A missing file is a clean first launch.
    pub fn load<T: DeserializeOwned>(&self) -> Result<Option<UiSavedState<T>>, UiStateError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() {
            return Err(UiStateError::InvalidPath(self.path.clone()));
        }
        if metadata.len() > MAX_UI_STATE_BYTES {
            return Err(UiStateError::StateTooLarge);
        }

        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        File::open(&self.path)?
            .take(MAX_UI_STATE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_UI_STATE_BYTES {
            return Err(UiStateError::StateTooLarge);
        }
        let envelope: UiSavedState<T> = serde_json::from_slice(&bytes)?;
        if envelope.format != UI_STATE_FORMAT || envelope.format_version != UI_STATE_FORMAT_VERSION
        {
            return Err(UiStateError::UnsupportedFormat {
                format: envelope.format,
                version: envelope.format_version,
            });
        }
        if envelope.app_id != self.app_id {
            return Err(UiStateError::AppMismatch {
                expected: self.app_id.clone(),
                received: envelope.app_id,
            });
        }
        if envelope.revision > MAX_SAFE_UI_INTEGER {
            return Err(UiStateError::InvalidRevision(envelope.revision));
        }
        Ok(Some(envelope))
    }

    /// Atomically replaces the state file with a complete versioned envelope.
    pub fn save<T: Serialize>(
        &self,
        state_schema_version: u32,
        revision: u64,
        state: &T,
    ) -> Result<UiSavedState<()>, UiStateError> {
        if revision > MAX_SAFE_UI_INTEGER {
            return Err(UiStateError::InvalidRevision(revision));
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| UiStateError::InvalidPath(self.path.clone()))?;
        if !fs::metadata(parent).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(UiStateError::InvalidPath(parent.to_owned()));
        }
        let saved_at_unix_ms = unix_time_millis()?;
        let envelope = UiSavedState {
            format: UI_STATE_FORMAT.to_owned(),
            format_version: UI_STATE_FORMAT_VERSION,
            app_id: self.app_id.clone(),
            app_version: self.app_version.clone(),
            state_schema_version,
            revision,
            saved_at_unix_ms,
            state,
        };
        let bytes = serde_json::to_vec(&envelope)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_UI_STATE_BYTES {
            return Err(UiStateError::StateTooLarge);
        }

        let suffix = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let filename = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| UiStateError::InvalidPath(self.path.clone()))?;
        let temporary = parent.join(format!(".{filename}.{}.{}.tmp", std::process::id(), suffix));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        let write_result = (|| -> Result<(), UiStateError> {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)?;
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;

        Ok(UiSavedState {
            format: UI_STATE_FORMAT.to_owned(),
            format_version: UI_STATE_FORMAT_VERSION,
            app_id: self.app_id.clone(),
            app_version: self.app_version.clone(),
            state_schema_version,
            revision,
            saved_at_unix_ms,
            state: (),
        })
    }
}

#[derive(Debug)]
pub enum UiStateError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidPath(PathBuf),
    StateTooLarge,
    UnsupportedFormat { format: String, version: u32 },
    AppMismatch { expected: String, received: String },
    InvalidRevision(u64),
    Clock,
}

impl fmt::Display for UiStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "App state I/O error: {error}"),
            Self::Json(error) => write!(formatter, "invalid App state JSON: {error}"),
            Self::InvalidPath(path) => {
                write!(formatter, "invalid App state path: {}", path.display())
            }
            Self::StateTooLarge => {
                write!(formatter, "App state exceeds {MAX_UI_STATE_BYTES} bytes")
            }
            Self::UnsupportedFormat { format, version } => {
                write!(
                    formatter,
                    "unsupported App state format {format:?} version {version}"
                )
            }
            Self::AppMismatch { expected, received } => write!(
                formatter,
                "App state belongs to {received:?}, not {expected:?}"
            ),
            Self::InvalidRevision(revision) => {
                write!(formatter, "invalid App state revision {revision}")
            }
            Self::Clock => formatter.write_str("system clock is before the Unix epoch"),
        }
    }
}

impl std::error::Error for UiStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for UiStateError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for UiStateError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

fn unix_time_millis() -> Result<u64, UiStateError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UiStateError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| UiStateError::Clock)
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Model {
        text: String,
    }

    #[test]
    fn state_round_trips_and_replaces_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(UI_STATE_FILENAME);
        let store = UiStateStore::new(&path, "com.unpeel.markdown", "0.1.0");
        assert!(store.load::<Model>().unwrap().is_none());

        store
            .save(
                3,
                7,
                &Model {
                    text: "first".into(),
                },
            )
            .unwrap();
        store
            .save(
                3,
                8,
                &Model {
                    text: "second".into(),
                },
            )
            .unwrap();
        let restored = store.load::<Model>().unwrap().unwrap();
        assert_eq!(restored.state_schema_version, 3);
        assert_eq!(restored.revision, 8);
        assert_eq!(restored.state.text, "second");
        assert_eq!(
            directory.path().read_dir().unwrap().count(),
            1,
            "temporary files must not survive a successful save"
        );
    }

    #[test]
    fn state_is_bound_to_the_app_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(UI_STATE_FILENAME);
        UiStateStore::new(&path, "app-one", "1")
            .save(
                1,
                1,
                &Model {
                    text: "state".into(),
                },
            )
            .unwrap();
        assert!(matches!(
            UiStateStore::new(&path, "app-two", "1").load::<Model>(),
            Err(UiStateError::AppMismatch { .. })
        ));
    }
}
