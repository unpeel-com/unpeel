//! Read-only, caller-relative projection of the Controller's durable pane
//! layout for MCP self-context.
//!
//! Pane membership and geometry remain Controller-owned. This module never
//! mutates `pane-layouts.json` and deliberately returns only the four spatial
//! neighbors of one Session in the shared `windows["main"]["local"]` split
//! tree. Ratios are consumed solely by the same artificial-grid navigation
//! algorithm the native and TUI Controllers implement; ratios, pane ids,
//! focus, zoom, and the rest of the layout are not exposed to agents.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

const STORAGE_VERSION: i64 = 1;
const LAYOUT_VERSION: i64 = 2;
const LEGACY_LAYOUT_VERSION: i64 = 1;
const WINDOW_ID: &str = "main";
const SCOPE_ID: &str = "local";
const MAX_LAYOUT_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneNeighborhood {
    pub(crate) window_id: String,
    pub(crate) scope_id: String,
    pub(crate) left: Option<String>,
    pub(crate) right: Option<String>,
    pub(crate) up: Option<String>,
    pub(crate) down: Option<String>,
}

/// Read the current Host's shared local Controller layout fresh from disk.
/// A missing file, absent slot, or caller outside a multi-pane group is a
/// normal `Ok(None)` result.
pub(crate) fn local_neighborhood(
    caller_session_id: &str,
) -> Result<Option<PaneNeighborhood>, String> {
    neighborhood_at(
        &crate::app_paths::unpeel_home().join("pane-layouts.json"),
        WINDOW_ID,
        SCOPE_ID,
        caller_session_id,
    )
}

fn neighborhood_at(
    path: &Path,
    window_id: &str,
    scope_id: &str,
    caller_session_id: &str,
) -> Result<Option<PaneNeighborhood>, String> {
    if caller_session_id.trim().is_empty() {
        return Err("caller Session id is empty".into());
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not open pane-layouts.json: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect pane-layouts.json: {error}"))?;
    if metadata.len() > MAX_LAYOUT_FILE_BYTES {
        return Err(format!(
            "pane-layouts.json exceeds the {MAX_LAYOUT_FILE_BYTES}-byte read limit"
        ));
    }
    // Enforce the bound on the read too: the Controller replaces this file
    // atomically, but a concurrent or malformed writer must not race the
    // metadata check into an unbounded MCP allocation.
    let mut raw = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_LAYOUT_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|error| format!("could not read pane-layouts.json: {error}"))?;
    if raw.len() as u64 > MAX_LAYOUT_FILE_BYTES {
        return Err(format!(
            "pane-layouts.json exceeds the {MAX_LAYOUT_FILE_BYTES}-byte read limit"
        ));
    }
    let storage: StorageFile = serde_json::from_slice(&raw)
        .map_err(|error| format!("pane-layouts.json is invalid: {error}"))?;
    if storage.version != STORAGE_VERSION {
        return Err(format!(
            "pane-layouts.json storage version {} is unsupported",
            storage.version
        ));
    }
    let Some(layout) = storage
        .windows
        .get(window_id)
        .and_then(|scopes| scopes.get(scope_id))
    else {
        return Ok(None);
    };
    let groups = layout.groups()?;
    for group in &groups {
        let Some(caller_pane_id) = group.root.pane_id_for_session(caller_session_id) else {
            continue;
        };
        return Ok(Some(PaneNeighborhood {
            window_id: window_id.to_string(),
            scope_id: scope_id.to_string(),
            left: group.root.spatial_neighbor(caller_pane_id, Direction::Left),
            right: group
                .root
                .spatial_neighbor(caller_pane_id, Direction::Right),
            up: group.root.spatial_neighbor(caller_pane_id, Direction::Up),
            down: group.root.spatial_neighbor(caller_pane_id, Direction::Down),
        }));
    }
    Ok(None)
}

#[derive(Deserialize)]
struct StorageFile {
    version: i64,
    #[serde(default)]
    windows: HashMap<String, HashMap<String, DurablePaneLayout>>,
}

#[derive(Deserialize)]
struct DurablePaneLayout {
    version: i64,
    #[serde(default)]
    groups: Value,
}

impl DurablePaneLayout {
    fn groups(&self) -> Result<Vec<DurablePaneGroup>, String> {
        match self.version {
            LAYOUT_VERSION => serde_json::from_value(self.groups.clone())
                .map_err(|error| format!("pane layout groups are invalid: {error}")),
            LEGACY_LAYOUT_VERSION => {
                let groups: Vec<LegacyPaneGroup> = serde_json::from_value(self.groups.clone())
                    .map_err(|error| format!("legacy pane layout groups are invalid: {error}"))?;
                Ok(groups
                    .into_iter()
                    .filter_map(LegacyPaneGroup::migrated)
                    .collect())
            }
            version => Err(format!(
                "pane layout version {version} is unsupported for agent context"
            )),
        }
    }
}

#[derive(Deserialize)]
struct DurablePaneGroup {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "representativePaneID")]
    _representative_pane_id: String,
    root: DurablePaneNode,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum DurablePaneNode {
    Pane(DurablePaneLeaf),
    Split(Box<DurablePaneSplit>),
}

#[derive(Deserialize)]
struct DurablePaneLeaf {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
}

#[derive(Deserialize)]
struct DurablePaneSplit {
    direction: SplitDirection,
    ratio: f64,
    left: DurablePaneNode,
    right: DurablePaneNode,
}

#[derive(Deserialize)]
struct LegacyPaneGroup {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "representativePaneID", alias = "representativePaneId")]
    _representative_pane_id: String,
    panes: Vec<LegacyPane>,
}

#[derive(Deserialize)]
struct LegacyPane {
    id: String,
    #[serde(rename = "sessionID", alias = "sessionId")]
    session_id: String,
    fraction: f64,
}

impl LegacyPaneGroup {
    fn migrated(self) -> Option<DurablePaneGroup> {
        if self.panes.len() < 2 {
            return None;
        }

        fn sanitized(value: f64) -> f64 {
            if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            }
        }

        fn fold(panes: &[LegacyPane], index: usize) -> DurablePaneNode {
            let pane = &panes[index];
            let leaf = DurablePaneNode::Pane(DurablePaneLeaf {
                id: pane.id.clone(),
                session_id: pane.session_id.clone(),
            });
            if index + 1 == panes.len() {
                return leaf;
            }
            let remaining = panes[index..]
                .iter()
                .map(|pane| sanitized(pane.fraction))
                .sum::<f64>();
            let ratio = if remaining > 0.0 {
                (sanitized(pane.fraction) / remaining).clamp(0.1, 0.9)
            } else {
                0.5
            };
            DurablePaneNode::Split(Box::new(DurablePaneSplit {
                direction: SplitDirection::Horizontal,
                ratio,
                left: leaf,
                right: fold(panes, index + 1),
            }))
        }

        Some(DurablePaneGroup {
            _id: self._id,
            _representative_pane_id: self._representative_pane_id,
            root: fold(&self.panes, 0),
        })
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Rect {
    fn max_x(self) -> f64 {
        self.x + self.width
    }

    fn max_y(self) -> f64 {
        self.y + self.height
    }
}

impl DurablePaneNode {
    fn pane_id_for_session(&self, session_id: &str) -> Option<&str> {
        match self {
            Self::Pane(pane) => (pane.session_id == session_id).then_some(pane.id.as_str()),
            Self::Split(split) => split
                .left
                .pane_id_for_session(session_id)
                .or_else(|| split.right.pane_id_for_session(session_id)),
        }
    }

    fn grid_dimensions(&self) -> (f64, f64) {
        match self {
            Self::Pane(_) => (1.0, 1.0),
            Self::Split(split) => {
                let (left_width, left_height) = split.left.grid_dimensions();
                let (right_width, right_height) = split.right.grid_dimensions();
                match split.direction {
                    SplitDirection::Horizontal => {
                        (left_width + right_width, left_height.max(right_height))
                    }
                    SplitDirection::Vertical => {
                        (left_width.max(right_width), left_height + right_height)
                    }
                }
            }
        }
    }

    fn leaf_slots<'a>(&'a self, bounds: Rect, slots: &mut Vec<(&'a DurablePaneLeaf, Rect)>) {
        match self {
            Self::Pane(pane) => slots.push((pane, bounds)),
            Self::Split(split) => {
                let ratio = if split.ratio.is_finite() {
                    split.ratio.clamp(0.1, 0.9)
                } else {
                    0.5
                };
                let (left_bounds, right_bounds) = match split.direction {
                    SplitDirection::Horizontal => (
                        Rect {
                            width: bounds.width * ratio,
                            ..bounds
                        },
                        Rect {
                            x: bounds.x + bounds.width * ratio,
                            width: bounds.width * (1.0 - ratio),
                            ..bounds
                        },
                    ),
                    SplitDirection::Vertical => (
                        Rect {
                            height: bounds.height * ratio,
                            ..bounds
                        },
                        Rect {
                            y: bounds.y + bounds.height * ratio,
                            height: bounds.height * (1.0 - ratio),
                            ..bounds
                        },
                    ),
                };
                split.left.leaf_slots(left_bounds, slots);
                split.right.leaf_slots(right_bounds, slots);
            }
        }
    }

    /// Same pure artificial-grid query as both Controller implementations:
    /// candidates clear the requested edge; top-left distance wins; preorder
    /// is the stable tie-break because equal distances never replace `best`.
    fn spatial_neighbor(&self, pane_id: &str, direction: Direction) -> Option<String> {
        let (width, height) = self.grid_dimensions();
        let mut slots = Vec::new();
        self.leaf_slots(
            Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            },
            &mut slots,
        );
        let reference = slots
            .iter()
            .find(|(pane, _)| pane.id == pane_id)
            .map(|(_, bounds)| *bounds)?;
        let mut best: Option<(&DurablePaneLeaf, f64)> = None;
        for (pane, bounds) in slots {
            if pane.id == pane_id {
                continue;
            }
            let qualifies = match direction {
                Direction::Left => bounds.max_x() <= reference.x,
                Direction::Right => bounds.x >= reference.max_x(),
                Direction::Up => bounds.max_y() <= reference.y,
                Direction::Down => bounds.y >= reference.max_y(),
            };
            if !qualifies {
                continue;
            }
            let dx = bounds.x - reference.x;
            let dy = bounds.y - reference.y;
            let distance = (dx * dx + dy * dy).sqrt();
            if best.is_none_or(|(_, current)| distance < current) {
                best = Some((pane, distance));
            }
        }
        best.map(|(pane, _)| pane.session_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_layout(path: &Path, root: serde_json::Value) {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "windows": {
                    "main": {
                        "local": {
                            "version": 2,
                            "groups": [{
                                "id": "group",
                                "representativePaneID": "self-pane",
                                "root": root,
                            }]
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn pane(id: &str, session_id: &str) -> serde_json::Value {
        json!({ "pane": { "id": id, "sessionID": session_id } })
    }

    fn split(
        direction: &str,
        ratio: f64,
        left: serde_json::Value,
        right: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "split": {
                "direction": direction,
                "ratio": ratio,
                "left": left,
                "right": right,
            }
        })
    }

    #[test]
    fn returns_caller_relative_neighbors_from_recursive_tree() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pane-layouts.json");
        write_layout(
            &path,
            split(
                "horizontal",
                0.5,
                pane("design-pane", "design-session"),
                split(
                    "vertical",
                    0.5,
                    pane("self-pane", "caller"),
                    pane("lower-pane", "lower-session"),
                ),
            ),
        );

        let context = neighborhood_at(&path, "main", "local", "caller")
            .unwrap()
            .expect("caller belongs to the pane tree");
        assert_eq!(context.left.as_deref(), Some("design-session"));
        assert_eq!(context.down.as_deref(), Some("lower-session"));
        assert_eq!(context.right, None);
        assert_eq!(context.up, None);
    }

    #[test]
    fn missing_layout_or_unmounted_caller_has_no_neighborhood() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pane-layouts.json");
        assert_eq!(
            neighborhood_at(&path, "main", "local", "caller").unwrap(),
            None
        );
        write_layout(
            &path,
            split(
                "horizontal",
                0.5,
                pane("one", "session-one"),
                pane("two", "session-two"),
            ),
        );
        assert_eq!(
            neighborhood_at(&path, "main", "local", "caller").unwrap(),
            None
        );
    }

    #[test]
    fn legacy_flat_layout_uses_the_controller_migration_shape() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pane-layouts.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1,
                "windows": {
                    "main": {
                        "local": {
                            "version": 1,
                            "groups": [{
                                "id": "legacy",
                                "representativePaneId": "left-pane",
                                "panes": [
                                    { "id": "left-pane", "sessionId": "design", "fraction": 0.5 },
                                    { "id": "right-pane", "sessionId": "caller", "fraction": 0.5 }
                                ]
                            }]
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let context = neighborhood_at(&path, "main", "local", "caller")
            .unwrap()
            .expect("legacy group migrates");
        assert_eq!(context.left.as_deref(), Some("design"));
    }

    #[test]
    fn unknown_layout_version_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pane-layouts.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1,
                "windows": { "main": { "local": { "version": 99, "groups": [] } } }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(neighborhood_at(&path, "main", "local", "caller")
            .unwrap_err()
            .contains("unsupported"));
    }
}
