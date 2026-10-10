//! The canonical project→index map, `project-map.toml` (#9169 PR-B1).
//!
//! Why: PR-A derives a project's index from the registry on every call
//! (ruling f7), so an operator cannot say "this clone is the one" and a
//! duplicate index cannot be set aside. trusty-search owns the map (ruling
//! f6); tm reads it through `search.project.map`.
//! What: the schema-1 file model — [`ProjectMap`] with its `[[projects]]` pins
//! and `[[parked]]` rows — the [`ProjectMapError`] every reader and writer
//! returns, and [`MapView`], the read-only slice the resolver consults. Disk
//! access lives in [`super::project_map_io`].
//! Test: `core::project_map::tests` (`project_map_tests.rs`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The file name, kept next to `indexes.toml` in the data dir.
pub const PROJECT_MAP_FILE: &str = "project-map.toml";

/// The only schema this build reads or writes.
pub const PROJECT_MAP_SCHEMA: u32 = 1;

/// The remedy every unreadable-map error carries.
pub const PROJECT_MAP_REMEDY: &str = "restore the file or run `trusty-search project rebuild-map`";

/// Who wrote a `[[projects]]` pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinnedBy {
    /// The first-boot migration pinned PR-A's f7 winner.
    Migration,
    /// An index was created for a group that had no pin.
    Create,
    /// An operator ran `project pin`.
    Operator,
}

/// Why a `[[parked]]` row exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkReason {
    /// The index's root is a git worktree.
    Worktree,
    /// The index's root no longer exists.
    Orphaned,
    /// An operator parked a same-root sibling.
    Operator,
}

/// One pin: the index a resolver group answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedProject {
    /// The resolver's group key: a repo identity or `root:<path>`.
    pub key: String,
    /// The pinned index id.
    pub index_id: String,
    /// The pinned index's root when it was pinned.
    pub root_path: PathBuf,
    /// Who wrote the pin.
    pub pinned_by: PinnedBy,
    /// When the pin was written.
    pub pinned_unix: u64,
}

/// One parked index: registered, corpus kept, never served or rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedIndex {
    /// The parked index id.
    pub index_id: String,
    /// The resolver group key the index belongs to.
    pub key: String,
    /// The index's root when it was parked.
    pub root_path: PathBuf,
    /// Why it was parked.
    pub reason: ParkReason,
    /// When it was parked.
    pub parked_unix: u64,
}

/// The whole `project-map.toml` file, schema 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMap {
    /// The file's schema version; [`PROJECT_MAP_SCHEMA`] on every write.
    pub schema: u32,
    /// One pin per resolver group key.
    #[serde(default)]
    pub projects: Vec<PinnedProject>,
    /// Parked indexes, one row per index id.
    #[serde(default)]
    pub parked: Vec<ParkedIndex>,
}

impl Default for ProjectMap {
    fn default() -> Self {
        Self {
            schema: PROJECT_MAP_SCHEMA,
            projects: Vec::new(),
            parked: Vec::new(),
        }
    }
}

/// Every way reading or writing the map fails. Each variant names the file.
#[derive(Debug, thiserror::Error)]
pub enum ProjectMapError {
    /// An I/O error other than NotFound while reading the map.
    #[error("project map {path} could not be read ({source}); {PROJECT_MAP_REMEDY}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The bytes are not a schema-1 map.
    #[error("project map {path} could not be parsed ({message}); {PROJECT_MAP_REMEDY}")]
    Parse { path: PathBuf, message: String },
    /// The file names a schema this build does not know.
    #[error(
        "project map {path} has schema {found}, this build reads schema \
         {PROJECT_MAP_SCHEMA}; {PROJECT_MAP_REMEDY}"
    )]
    Schema { path: PathBuf, found: u32 },
    /// The cross-process lock could not be taken; nothing was written.
    #[error("could not lock project map {path} ({source}); nothing was written")]
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Serialising or publishing the map failed; the file keeps its bytes.
    #[error("could not write project map {path} ({message})")]
    Write { path: PathBuf, message: String },
    /// `indexes.toml` was unreadable, so no migration ran and no map was written.
    #[error("index registry unreadable, project map {path} not written: {message}")]
    Registry { path: PathBuf, message: String },
}

impl ProjectMap {
    /// The pin for group `key`, if any.
    pub fn pin_for(&self, key: &str) -> Option<&PinnedProject> {
        self.projects.iter().find(|p| p.key == key)
    }

    /// The parked row for `index_id`, if the index is parked.
    pub fn parked_row(&self, index_id: &str) -> Option<&ParkedIndex> {
        self.parked.iter().find(|p| p.index_id == index_id)
    }

    /// Whether `index_id` is parked.
    pub fn is_parked(&self, index_id: &str) -> bool {
        self.parked_row(index_id).is_some()
    }

    /// Every parked index id.
    pub fn parked_ids(&self) -> HashSet<String> {
        self.parked.iter().map(|p| p.index_id.clone()).collect()
    }

    /// Set the pin for `pin.key`, replacing any earlier one. Returns the
    /// replaced pin's index id.
    pub fn set_pin(&mut self, pin: PinnedProject) -> Option<String> {
        match self.projects.iter_mut().find(|p| p.key == pin.key) {
            Some(existing) => Some(std::mem::replace(existing, pin).index_id),
            None => {
                self.projects.push(pin);
                None
            }
        }
    }

    /// Park `row.index_id`, replacing an earlier row for the same id.
    pub fn park(&mut self, row: ParkedIndex) {
        self.parked.retain(|p| p.index_id != row.index_id);
        self.parked.push(row);
    }

    /// Clear `index_id`'s parked row; returns it when there was one.
    pub fn unpark(&mut self, index_id: &str) -> Option<ParkedIndex> {
        let at = self.parked.iter().position(|p| p.index_id == index_id)?;
        Some(self.parked.remove(at))
    }

    /// Drop every pin naming `index_id`; returns the dropped pins.
    ///
    /// Why: #9169 — a parked or deleted index can never answer for its
    /// group, so a pin left on it would name a dead id.
    /// Test: `parking_a_pinned_orphan_names_the_live_winner_and_clears_the_pin`,
    /// `a_deleted_parked_index_recreated_at_its_root_is_served`.
    pub fn unpin_index(&mut self, index_id: &str) -> Vec<PinnedProject> {
        let (dropped, kept) = std::mem::take(&mut self.projects)
            .into_iter()
            .partition(|p| p.index_id == index_id);
        self.projects = kept;
        dropped
    }

    /// The resolver's read-only slice of the map.
    pub fn view(&self) -> MapView {
        MapView {
            pins: self
                .projects
                .iter()
                .map(|p| (p.key.clone(), p.index_id.clone()))
                .collect(),
            parked: self.parked_ids(),
        }
    }
}

/// What the resolver needs from the map: the pin per group key and the
/// parked ids (#9169).
///
/// Why: the resolver must not know the file format, and its tests build a
/// view in one line.
/// What: `pins` maps a group key to its pinned index id; a parked id never
/// wins a resolve. The default view is PR-A's derived resolver.
/// Test: `a_pin_overrides_the_f7_winner_of_its_group`,
/// `a_parked_index_never_wins_and_an_exact_parked_id_yields_to_its_group`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapView {
    /// Group key → pinned index id.
    pub pins: HashMap<String, String>,
    /// Parked index ids.
    pub parked: HashSet<String>,
}

impl MapView {
    /// Whether `index_id` is parked.
    pub fn is_parked(&self, index_id: &str) -> bool {
        self.parked.contains(index_id)
    }

    /// The index pinned for group `key`.
    pub fn pin(&self, key: &str) -> Option<&str> {
        self.pins.get(key).map(String::as_str)
    }
}

/// Seconds since the Unix epoch; 0 for a clock before it.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "project_map_tests.rs"]
mod tests;
