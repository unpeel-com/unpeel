use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::layout::{Position, Rect};
use serde::{Deserialize, Serialize};

/// Session-local map of terminal cells that accept semantic file drops.
pub const DROP_TARGET_MAP_FILENAME: &str = "terminal-drop-target-map.json";
/// Session-local event written by Unpeel's native terminal destination.
pub const DROP_TARGET_EVENT_FILENAME: &str = "terminal-drop-target-event.json";

const DROP_TARGET_VERSION: u8 = 1;
const DROP_TARGET_HEARTBEAT: Duration = Duration::from_secs(2);
const MAXIMUM_MAP_BYTES: usize = 64 * 1024;
const MAXIMUM_EVENT_BYTES: usize = 1024 * 1024;
const MAXIMUM_EVENT_AGE: u64 = 5_000;
const MAXIMUM_FUTURE_SKEW: u64 = 5_000;

/// A terminal rectangle that accepts file/folder drag hover and drop events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropTargetRegion {
    pub area: Rect,
}

/// Semantic native drag event delivered to a hosted TUI App.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTargetEvent {
    /// A supported drag is hovering over this terminal cell.
    Hover { position: Position },
    /// The drag left every registered target.
    Leave,
    /// References were dropped at a terminal cell. `text` preserves the
    /// Host's normal path shortening, image handling, and quoting behavior.
    Drop {
        position: Position,
        text: String,
        references: Vec<String>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct DropTargetMap {
    version: u8,
    pid: u32,
    updated_at: u64,
    regions: Vec<WireRegion>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireRegion {
    screen_row: u16,
    start_column: u16,
    end_row: u32,
    end_column: u32,
}

#[derive(Debug, Deserialize)]
struct WireEvent {
    version: u8,
    event_id: String,
    updated_at: u64,
    kind: String,
    screen_row: Option<u16>,
    column: Option<u16>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    references: Vec<String>,
}

/// Publishes drop destinations for a Ratatui frame and polls native hover/drop
/// events from the current Unpeel Session.
///
/// The filesystem contract is deliberately Session-local and inert in an
/// ordinary terminal. Apps register semantic rectangles after rendering,
/// commit the frame, and poll from their normal event-loop tick.
#[derive(Debug)]
pub struct DropTargetSurface {
    session_directory: Option<PathBuf>,
    regions: Vec<DropTargetRegion>,
    last_write: Option<Instant>,
    last_event_id: Option<String>,
}

impl DropTargetSurface {
    /// Detects the current hosted Session from Unpeel's process environment.
    #[must_use]
    pub fn detect() -> Self {
        let session_directory = std::env::var_os("UNPEEL_SESSION_ID")
            .filter(|id| !id.is_empty())
            .and_then(|_| std::env::var_os("UNPEEL_SESSION_DIR"))
            .map(PathBuf::from)
            .filter(|path| path.is_dir());
        Self::with_optional_session_directory(session_directory)
    }

    /// Creates an active surface for a known hosted-session directory.
    #[must_use]
    pub fn for_session_directory(path: impl Into<PathBuf>) -> Self {
        Self::with_optional_session_directory(Some(path.into()))
    }

    /// Creates an inert surface for standalone use.
    #[must_use]
    pub fn disabled() -> Self {
        Self::with_optional_session_directory(None)
    }

    fn with_optional_session_directory(session_directory: Option<PathBuf>) -> Self {
        let last_event_id = session_directory
            .as_deref()
            .and_then(read_wire_event)
            .map(|event| event.event_id);
        Self {
            session_directory,
            regions: Vec::new(),
            last_write: None,
            last_event_id,
        }
    }

    /// Whether native semantic drop routing is available.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.session_directory.is_some()
    }

    /// Clears regions from the previous terminal frame.
    pub fn begin_frame(&mut self) {
        self.regions.clear();
    }

    /// Registers a destination rectangle from the current rendered frame.
    pub fn register(&mut self, area: Rect) -> bool {
        if area.is_empty() {
            return false;
        }
        self.regions.push(DropTargetRegion { area });
        true
    }

    /// Regions collected during the current frame.
    #[must_use]
    pub fn regions(&self) -> &[DropTargetRegion] {
        &self.regions
    }

    /// Atomically publishes the current frame's destination map.
    pub fn commit(&mut self) -> io::Result<()> {
        self.write_map()
    }

    /// Keeps an unchanged destination map fresh without repainting.
    pub fn heartbeat(&mut self) -> io::Result<bool> {
        let should_write = self
            .last_write
            .is_some_and(|written| written.elapsed() >= DROP_TARGET_HEARTBEAT);
        if should_write {
            self.write_map()?;
        }
        Ok(should_write)
    }

    /// Returns the newest native drag event exactly once.
    pub fn poll(&mut self) -> io::Result<Option<DropTargetEvent>> {
        let Some(directory) = self.session_directory.as_deref() else {
            return Ok(None);
        };
        let Some(event) = read_wire_event(directory) else {
            return Ok(None);
        };
        if self.last_event_id.as_deref() == Some(event.event_id.as_str()) {
            return Ok(None);
        }
        self.last_event_id = Some(event.event_id.clone());
        if !wire_event_is_fresh(&event, now_milliseconds()) {
            return Ok(None);
        }
        Ok(match event.kind.as_str() {
            "hover" => wire_position(&event).map(|position| DropTargetEvent::Hover { position }),
            "leave" => Some(DropTargetEvent::Leave),
            "drop" => wire_position(&event).map(|position| DropTargetEvent::Drop {
                position,
                text: event.text,
                references: event.references,
            }),
            _ => None,
        })
    }

    fn write_map(&mut self) -> io::Result<()> {
        let Some(directory) = &self.session_directory else {
            return Ok(());
        };
        let regions = self
            .regions
            .iter()
            .map(|region| WireRegion {
                screen_row: region.area.y,
                start_column: region.area.x,
                end_row: u32::from(region.area.y) + u32::from(region.area.height),
                end_column: u32::from(region.area.x) + u32::from(region.area.width),
            })
            .collect();
        let body = serde_json::to_vec(&DropTargetMap {
            version: DROP_TARGET_VERSION,
            pid: std::process::id(),
            updated_at: now_milliseconds(),
            regions,
        })?;
        if body.len() > MAXIMUM_MAP_BYTES {
            self.remove_own_marker();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "terminal drop target map is {} bytes; maximum is {MAXIMUM_MAP_BYTES}",
                    body.len()
                ),
            ));
        }
        let marker = directory.join(DROP_TARGET_MAP_FILENAME);
        let temporary = directory.join(format!(
            ".{DROP_TARGET_MAP_FILENAME}.{}.tmp",
            std::process::id()
        ));
        if let Err(error) =
            fs::write(&temporary, body).and_then(|()| fs::rename(&temporary, marker))
        {
            let _ = fs::remove_file(temporary);
            return Err(error);
        }
        self.last_write = Some(Instant::now());
        Ok(())
    }

    fn remove_own_marker(&self) {
        let Some(directory) = &self.session_directory else {
            return;
        };
        let marker = directory.join(DROP_TARGET_MAP_FILENAME);
        let belongs_to_this_process = fs::read(&marker)
            .ok()
            .filter(|body| body.len() <= MAXIMUM_MAP_BYTES)
            .and_then(|body| serde_json::from_slice::<DropTargetMap>(&body).ok())
            .is_some_and(|map| map.pid == std::process::id());
        if belongs_to_this_process {
            let _ = fs::remove_file(marker);
        }
    }
}

impl Drop for DropTargetSurface {
    fn drop(&mut self) {
        self.remove_own_marker();
    }
}

fn read_wire_event(directory: &Path) -> Option<WireEvent> {
    let body = fs::read(directory.join(DROP_TARGET_EVENT_FILENAME)).ok()?;
    if body.len() > MAXIMUM_EVENT_BYTES {
        return None;
    }
    serde_json::from_slice(&body).ok()
}

fn wire_event_is_fresh(event: &WireEvent, now: u64) -> bool {
    event.version == DROP_TARGET_VERSION
        && !event.event_id.is_empty()
        && event.updated_at <= now.saturating_add(MAXIMUM_FUTURE_SKEW)
        && now <= event.updated_at.saturating_add(MAXIMUM_EVENT_AGE)
}

fn wire_position(event: &WireEvent) -> Option<Position> {
    Some(Position::new(event.column?, event.screen_row?))
}

fn now_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn target_map_is_frame_scoped_and_removed_with_its_owner() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join(DROP_TARGET_MAP_FILENAME);
        {
            let mut surface = DropTargetSurface::for_session_directory(directory.path());
            surface.begin_frame();
            assert!(surface.register(Rect::new(2, 3, 20, 8)));
            surface.commit().unwrap();
            let map: DropTargetMap = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
            assert_eq!(map.regions.len(), 1);
            assert_eq!(map.regions[0].screen_row, 3);
            assert_eq!(map.regions[0].end_row, 11);
            assert_eq!(map.regions[0].start_column, 2);
            assert_eq!(map.regions[0].end_column, 22);
        }
        assert!(!marker.exists());
    }

    #[test]
    fn native_events_are_delivered_once_with_terminal_coordinates() {
        let directory = tempfile::tempdir().unwrap();
        let mut surface = DropTargetSurface::for_session_directory(directory.path());
        let event = json!({
            "version": 1,
            "event_id": "event-1",
            "updated_at": now_milliseconds(),
            "kind": "drop",
            "screen_row": 7,
            "column": 11,
            "text": "'folder/file name.md'",
            "references": ["/tmp/folder/file name.md"]
        });
        fs::write(
            directory.path().join(DROP_TARGET_EVENT_FILENAME),
            serde_json::to_vec(&event).unwrap(),
        )
        .unwrap();

        assert_eq!(
            surface.poll().unwrap(),
            Some(DropTargetEvent::Drop {
                position: Position::new(11, 7),
                text: "'folder/file name.md'".to_string(),
                references: vec!["/tmp/folder/file name.md".to_string()],
            })
        );
        assert_eq!(surface.poll().unwrap(), None);
    }

    #[test]
    fn existing_events_are_ignored_when_a_surface_starts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(DROP_TARGET_EVENT_FILENAME),
            serde_json::to_vec(&json!({
                "version": 1,
                "event_id": "old",
                "updated_at": now_milliseconds(),
                "kind": "hover",
                "screen_row": 1,
                "column": 2
            }))
            .unwrap(),
        )
        .unwrap();
        let mut surface = DropTargetSurface::for_session_directory(directory.path());
        assert_eq!(surface.poll().unwrap(), None);
    }
}
