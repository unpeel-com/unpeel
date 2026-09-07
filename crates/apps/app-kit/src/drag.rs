use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

/// Session-local presentation marker consumed by Unpeel's native terminal.
pub const DRAG_MAP_FILENAME: &str = "terminal-drag-map.json";

const DRAG_MAP_VERSION: u8 = 1;
const DRAG_MAP_HEARTBEAT: Duration = Duration::from_secs(2);
const MAXIMUM_MAP_BYTES: usize = 64 * 1024;

/// A terminal rectangle that represents a Host-local file or directory.
///
/// Coordinates are absolute cells in the terminal viewport. Registering a
/// rectangle taller than one row maps every row in the rectangle to `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragRegion {
    pub area: Rect,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct DragMap {
    version: u8,
    pid: u32,
    updated_at: u64,
    rows: Vec<DragRow>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DragRow {
    screen_row: u16,
    start_column: u16,
    end_column: u32,
    path: PathBuf,
}

/// Collects draggable Ratatui regions and publishes them for the current
/// Unpeel hosted session.
///
/// Outside an Unpeel session this type is deliberately inert, so the same TUI
/// binary remains usable in an ordinary terminal.
#[derive(Debug)]
pub struct DragSurface {
    session_directory: Option<PathBuf>,
    regions: Vec<DragRegion>,
    last_write: Option<Instant>,
}

impl DragSurface {
    /// Detects the current hosted session from Unpeel's process environment.
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
    ///
    /// Most Apps should use [`Self::detect`]. This constructor is useful to
    /// adapters that already receive the session directory explicitly and to
    /// deterministic tests.
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
        Self {
            session_directory,
            regions: Vec::new(),
            last_write: None,
        }
    }

    /// Whether this process can publish drag regions to an Unpeel session.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.session_directory.is_some()
    }

    /// The detected session directory, when running under Unpeel.
    #[must_use]
    pub fn session_directory(&self) -> Option<&Path> {
        self.session_directory.as_deref()
    }

    /// Clears regions from the previous terminal frame.
    ///
    /// Call once immediately before `Terminal::draw`.
    pub fn begin_frame(&mut self) {
        self.regions.clear();
    }

    /// Registers an absolute Host-local path for a terminal rectangle.
    ///
    /// Returns `false` for empty rectangles and relative paths. The native
    /// receiver independently verifies that the path still exists before it
    /// starts a drag.
    pub fn register(&mut self, area: Rect, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if area.is_empty() || !path.is_absolute() {
            return false;
        }
        self.regions.push(DragRegion {
            area,
            path: path.to_path_buf(),
        });
        true
    }

    /// Regions collected during the current frame.
    #[must_use]
    pub fn regions(&self) -> &[DragRegion] {
        &self.regions
    }

    /// Atomically publishes all regions collected for the current frame.
    ///
    /// This is a no-op outside Unpeel. Call after `Terminal::draw` succeeds so
    /// the semantic map always describes the terminal cells now on screen.
    pub fn commit(&mut self) -> io::Result<()> {
        self.write_map()
    }

    /// Refreshes a committed map without repainting the terminal.
    ///
    /// Unpeel rejects stale maps. Call this from an idle event-loop tick; disk
    /// writes are internally limited to one every two seconds.
    pub fn heartbeat(&mut self) -> io::Result<bool> {
        let should_write = self
            .last_write
            .is_some_and(|written| written.elapsed() >= DRAG_MAP_HEARTBEAT);
        if should_write {
            self.write_map()?;
        }
        Ok(should_write)
    }

    fn write_map(&mut self) -> io::Result<()> {
        let Some(directory) = &self.session_directory else {
            return Ok(());
        };
        let rows = self
            .regions
            .iter()
            .flat_map(|region| {
                let start_column = region.area.x;
                let end_column = u32::from(region.area.x) + u32::from(region.area.width);
                (0..region.area.height).map(move |offset| DragRow {
                    screen_row: region.area.y.saturating_add(offset),
                    start_column,
                    end_column,
                    path: region.path.clone(),
                })
            })
            .collect();
        let body = serde_json::to_vec(&DragMap {
            version: DRAG_MAP_VERSION,
            pid: std::process::id(),
            updated_at: now_milliseconds(),
            rows,
        })?;
        if body.len() > MAXIMUM_MAP_BYTES {
            self.remove_own_marker();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "terminal drag map is {} bytes; maximum is {MAXIMUM_MAP_BYTES}",
                    body.len()
                ),
            ));
        }

        let marker = directory.join(DRAG_MAP_FILENAME);
        let temporary = directory.join(format!(".{DRAG_MAP_FILENAME}.{}.tmp", std::process::id()));
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
        let marker = directory.join(DRAG_MAP_FILENAME);
        let belongs_to_this_process = fs::read(&marker)
            .ok()
            .filter(|body| body.len() <= MAXIMUM_MAP_BYTES)
            .and_then(|body| serde_json::from_slice::<DragMap>(&body).ok())
            .is_some_and(|map| map.pid == std::process::id());
        if belongs_to_this_process {
            let _ = fs::remove_file(marker);
        }
    }
}

impl Drop for DragSurface {
    fn drop(&mut self) {
        self.remove_own_marker();
    }
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

    #[test]
    fn commit_writes_each_grid_row_and_drop_removes_the_marker() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join(DRAG_MAP_FILENAME);
        {
            let mut surface = DragSurface::for_session_directory(directory.path());
            surface.begin_frame();
            assert!(surface.register(Rect::new(3, 4, 12, 2), "/tmp/a folder"));
            surface.commit().unwrap();

            let map: DragMap = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
            assert_eq!(map.version, 1);
            assert_eq!(map.rows.len(), 2);
            assert_eq!(map.rows[0].screen_row, 4);
            assert_eq!(map.rows[1].screen_row, 5);
            assert_eq!(map.rows[0].start_column, 3);
            assert_eq!(map.rows[0].end_column, 15);
            assert_eq!(map.rows[0].path, Path::new("/tmp/a folder"));
        }
        assert!(!marker.exists());
    }

    #[test]
    fn relative_paths_and_empty_areas_fail_closed() {
        let mut surface = DragSurface::disabled();
        assert!(!surface.register(Rect::new(0, 0, 10, 1), "relative/path"));
        assert!(!surface.register(Rect::new(0, 0, 0, 1), "/tmp/item"));
        assert!(surface.regions().is_empty());
    }

    #[test]
    fn disabled_surface_is_a_clean_no_op() {
        let mut surface = DragSurface::disabled();
        surface.begin_frame();
        assert!(surface.register(Rect::new(0, 0, 4, 1), "/tmp/item"));
        surface.commit().unwrap();
        assert!(!surface.heartbeat().unwrap());
    }
}
