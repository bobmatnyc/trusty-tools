//! The daemon side of `project-map.toml`: where it lives, the first-boot
//! migration, and the park / pin decisions (#9169 PR-B1).
//!
//! Why: the map's rules must be one implementation shared by the socket
//! methods, the boot path and the operator CLI, or the three would disagree
//! about which index a project is.
//! What: [`MapContext`] captures the registry once per call; [`ensure_map`]
//! reads the map and migrates when it is absent; [`park_decision`] and
//! [`pin_decision`] apply the park and pin rules; [`retain_unparked`] keeps a
//! prune off parked indexes. Every function that reads a root or a corpus is
//! blocking and runs on the blocking pool.
//! Test: `service::project_map_state_tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::project_map::{
    now_unix, ParkReason, ParkedIndex, PinnedBy, PinnedProject, ProjectMap, ProjectMapError,
};
use crate::core::project_map_io;
use crate::core::registry::IndexHandle;
use crate::service::persistence::PersistedIndex;
use crate::service::project_resolve::{
    gather_candidates, group_winners, resolve_pinned, Candidate, Disk, LiveDisk, ProjectQuery,
    RootKind,
};
use crate::service::server::SearchAppState;

/// One call's view of the registry and the map's path.
pub struct MapContext {
    /// The `indexes.toml` this daemon reads.
    pub registry_path: PathBuf,
    /// The `project-map.toml` beside it.
    pub map_path: PathBuf,
    /// The persisted registrations.
    pub persisted: Vec<PersistedIndex>,
    /// The resident handles.
    pub handles: Vec<Arc<IndexHandle>>,
}

impl MapContext {
    /// Capture `state`'s registry path, its rows and its resident handles.
    ///
    /// # Errors
    ///
    /// [`ProjectMapError::Registry`] when `indexes.toml` cannot be read — an
    /// unreadable registry never reads as an empty one (#4317).
    pub fn capture(state: &SearchAppState) -> Result<Self, ProjectMapError> {
        let registry_path = registry_path(state.registry_path_override.as_deref())?;
        let map_path = project_map_io::map_path_beside(&registry_path);
        let persisted = crate::service::persistence::load_index_registry_at(&registry_path)
            .map_err(|e| ProjectMapError::Registry {
                path: map_path.clone(),
                message: format!("{e:#}"),
            })?;
        Ok(Self {
            registry_path,
            map_path,
            persisted,
            handles: state.registry.list_handles(),
        })
    }

    /// `(id, root)` of every resident handle.
    pub fn resident(&self) -> Vec<(String, PathBuf)> {
        self.handles
            .iter()
            .map(|h| (h.id.0.clone(), h.root_path.clone()))
            .collect()
    }

    /// The resolver's candidates, unprobed.
    pub fn candidates(&self) -> Vec<Candidate> {
        gather_candidates(&self.persisted, &self.resident())
    }

    /// The production [`Disk`], reading resident stamps through their stores.
    /// Call it on a blocking thread: a stamp read blocks on `runtime`.
    pub fn disk(&self, runtime: Option<tokio::runtime::Handle>) -> LiveDisk {
        LiveDisk {
            names: crate::service::constants::ephemeral_dir_names(),
            resident: self
                .handles
                .iter()
                .map(|h| (h.id.0.clone(), Arc::clone(h)))
                .collect(),
            runtime,
        }
    }
}

/// The registry file: the test override, else `<data_dir>/indexes.toml`.
///
/// # Errors
///
/// [`ProjectMapError::Registry`] when the data dir cannot be resolved.
pub fn registry_path(override_path: Option<&Path>) -> Result<PathBuf, ProjectMapError> {
    match override_path {
        Some(path) => Ok(path.to_path_buf()),
        None => crate::service::persistence::indexes_toml_path().map_err(|e| {
            ProjectMapError::Registry {
                path: PathBuf::from(crate::core::project_map::PROJECT_MAP_FILE),
                message: format!("{e:#}"),
            }
        }),
    }
}

/// The map this daemon reads, migrated on first use (#9169).
///
/// Why: the first boot after the upgrade has no map; its answers must equal
/// PR-A's derived ones, and later boots must find the same map.
/// What: [`project_map_io::load`] (fail closed); a missing file is migrated
/// under the write lock — re-checked there, so two racing first calls write
/// once — from `candidates`. Never touches `indexes.toml` or `projects.json`.
/// Test: `migration_pins_every_groups_f7_winner_and_parks_nothing`,
/// `migration_persists_and_a_second_run_changes_nothing`.
pub fn ensure_map(
    map_path: &Path,
    candidates: &[Candidate],
    disk: &impl Disk,
) -> Result<ProjectMap, ProjectMapError> {
    if let Some(map) = project_map_io::load(map_path)? {
        return Ok(map);
    }
    project_map_io::update(map_path, |current| match current {
        Some(map) => Ok((None, map)),
        None => {
            let map = migrated(candidates, disk);
            tracing::info!(
                path = %map_path.display(),
                pins = map.projects.len(),
                "project-map: migrated from the registry (#9169)"
            );
            Ok((Some(map.clone()), map))
        }
    })
}

/// A fresh map pinning every group's PR-A winner; nothing parked.
pub fn migrated(candidates: &[Candidate], disk: &impl Disk) -> ProjectMap {
    let now = now_unix();
    let projects = group_winners(candidates, disk)
        .into_iter()
        .map(|(key, c)| PinnedProject {
            key,
            index_id: c.index_id,
            root_path: c.root_path,
            pinned_by: PinnedBy::Migration,
            pinned_unix: now,
        })
        .collect();
    ProjectMap {
        projects,
        ..ProjectMap::default()
    }
}

/// Why a park or pin was refused; the RPC layer renders it.
#[derive(Debug, thiserror::Error)]
pub enum MapRefusal {
    /// The map could not be read or written.
    #[error(transparent)]
    Map(#[from] ProjectMapError),
    /// No registration has this id.
    #[error("unknown index: {0}")]
    UnknownIndex(String),
    /// The index may not be parked or pinned; the message says why.
    #[error("{0}")]
    Refused(String),
    /// Unpark of an index that is not parked.
    #[error("index {0} is not parked")]
    NotParked(String),
}

/// The probed candidate for `index_id` and its group key.
fn target(
    index_id: &str,
    candidates: &[Candidate],
    disk: &impl Disk,
) -> Result<(Candidate, String), MapRefusal> {
    let found = candidates
        .iter()
        .find(|c| c.index_id == index_id)
        .ok_or_else(|| MapRefusal::UnknownIndex(index_id.to_string()))?;
    let probed = Candidate {
        kind: disk.kind(found),
        ..found.clone()
    };
    let key = probed.group_key();
    Ok((probed, key))
}

/// The index `key`'s group answers with under `map`, if any.
fn group_answer(
    key: &str,
    candidates: &[Candidate],
    disk: &impl Disk,
    map: &ProjectMap,
) -> Option<String> {
    let query = ProjectQuery::Identity(key.to_string());
    resolve_pinned(&query, candidates, disk, &map.view())
        .ok()
        .map(|r| r.index.index_id)
}

/// Park `index_id`, recording why (#9169).
///
/// Why: a duplicate index costs RAM, a watcher and reindexes while serving
/// nothing a caller should read; parking sets it aside without deleting it.
/// What: a worktree root parks as `worktree`, a missing root as `orphaned`,
/// and an index sharing its root with another registration as `operator` —
/// unless it is its group's live index. A second main checkout, or any other
/// root, is refused: the map cannot tell which clone an operator works in.
/// Idempotent. Returns the row and the group's live index afterwards.
/// Test: `park_refuses_a_main_checkout_and_records_a_worktree`,
/// `park_and_unpark_keep_the_corpus_and_queue_a_reindex`.
pub fn park_decision(
    map_path: &Path,
    index_id: &str,
    candidates: &[Candidate],
    disk: &impl Disk,
) -> Result<(ParkedIndex, Option<String>), MapRefusal> {
    let (probed, key) = target(index_id, candidates, disk)?;
    let shares_root = candidates
        .iter()
        .any(|c| c.index_id != index_id && c.root_path == probed.root_path);
    let current = ensure_map(map_path, candidates, disk)?;
    let reason = match probed.kind {
        RootKind::Worktree => ParkReason::Worktree,
        RootKind::Orphaned => ParkReason::Orphaned,
        _ if shares_root => {
            if group_answer(&key, candidates, disk, &current).as_deref() == Some(index_id) {
                return Err(MapRefusal::Refused(format!(
                    "{index_id} is the live index of {key}; pin another index of the \
                     group with `trusty-search project pin` first"
                )));
            }
            ParkReason::Operator
        }
        RootKind::MainCheckout => {
            return Err(MapRefusal::Refused(format!(
                "{index_id} is a main checkout ({}); a second main checkout is \
                 ambiguous, so it is never parked — pin the one you use with \
                 `trusty-search project pin`",
                probed.root_path.display()
            )))
        }
        other => {
            return Err(MapRefusal::Refused(format!(
                "{index_id} ({other:?} root {}) is not a park candidate: only a \
                 worktree, an orphaned root or a same-root sibling parks",
                probed.root_path.display()
            )))
        }
    };
    let row = ParkedIndex {
        index_id: index_id.to_string(),
        key: key.clone(),
        root_path: probed.root_path.clone(),
        reason,
        parked_unix: now_unix(),
    };
    let written = project_map_io::update(map_path, |map| {
        let mut map = map.unwrap_or(current);
        if let Some(existing) = map.parked_row(index_id) {
            let existing = existing.clone();
            return Ok((None, (existing, map)));
        }
        map.park(row.clone());
        Ok((Some(map.clone()), (row, map)))
    })?;
    let (row, map) = written;
    let canonical = group_answer(&row.key, candidates, disk, &map);
    Ok((row, canonical))
}

/// Pin `index_id` as its group's index (`pinned_by = operator`).
///
/// What: refuses an unknown, parked, worktree or orphaned index — none can
/// answer for a project. Returns the pin and the id it replaced.
/// Test: `pin_moves_a_groups_answer_and_refuses_a_worktree`.
pub fn pin_decision(
    map_path: &Path,
    index_id: &str,
    pinned_by: PinnedBy,
    candidates: &[Candidate],
    disk: &impl Disk,
) -> Result<(PinnedProject, Option<String>), MapRefusal> {
    let (probed, key) = target(index_id, candidates, disk)?;
    if !probed.kind.can_win() {
        return Err(MapRefusal::Refused(format!(
            "{index_id} ({:?} root) can never answer for a project, so it is not pinned",
            probed.kind
        )));
    }
    let current = ensure_map(map_path, candidates, disk)?;
    let pin = PinnedProject {
        key,
        index_id: index_id.to_string(),
        root_path: probed.root_path,
        pinned_by,
        pinned_unix: now_unix(),
    };
    project_map_io::update(map_path, |map| {
        let mut map = map.unwrap_or(current);
        if map.is_parked(index_id) {
            return Ok((None, None));
        }
        let previous = map.set_pin(pin.clone());
        Ok((Some(map), Some((pin, previous))))
    })?
    .ok_or_else(|| {
        MapRefusal::Refused(format!("{index_id} is parked; unpark it before pinning it"))
    })
}

/// Clear `index_id`'s parked row.
///
/// # Errors
///
/// [`MapRefusal::NotParked`] when there is no row; a map error otherwise.
pub fn unpark_decision(map_path: &Path, index_id: &str) -> Result<ParkedIndex, MapRefusal> {
    project_map_io::update(map_path, |map| {
        let Some(mut map) = map else {
            return Ok((None, None));
        };
        let removed = map.unpark(index_id);
        let next = removed.is_some().then(|| map.clone());
        Ok((next, removed))
    })?
    .ok_or_else(|| MapRefusal::NotParked(index_id.to_string()))
}

/// Pin the group of a just-created index when it has no pin (`create`).
///
/// What: an absent map is migrated (which pins the new group too); an
/// existing map gains a pin only for a group that has none and whose resolve
/// picks a live index.
/// Test: `creating_an_index_for_an_unpinned_group_pins_it`.
pub fn pin_on_create(
    map_path: &Path,
    index_id: &str,
    candidates: &[Candidate],
    disk: &impl Disk,
) -> Result<Option<PinnedProject>, MapRefusal> {
    let (_, key) = target(index_id, candidates, disk)?;
    let current = ensure_map(map_path, candidates, disk)?;
    if current.pin_for(&key).is_some() {
        return Ok(None);
    }
    let Some(winner) = group_answer(&key, candidates, disk, &current) else {
        return Ok(None);
    };
    let Some(found) = candidates.iter().find(|c| c.index_id == winner) else {
        return Ok(None);
    };
    let pin = PinnedProject {
        key: key.clone(),
        index_id: winner,
        root_path: found.root_path.clone(),
        pinned_by: PinnedBy::Create,
        pinned_unix: now_unix(),
    };
    project_map_io::update(map_path, |map| {
        let mut map = map.unwrap_or(current);
        if map.pin_for(&key).is_some() {
            return Ok((None, None));
        }
        map.set_pin(pin.clone());
        Ok((Some(map), Some(pin)))
    })
    .map_err(MapRefusal::from)
}

/// `trusty-search project rebuild-map`: move the map aside, then migrate it
/// afresh from `registry_path` (#9169).
///
/// Why: the operator's way past a fail-closed map; the bad bytes are kept.
/// What: reads the registry first (an unreadable one moves nothing and writes
/// nothing), then [`project_map_io::quarantine_and_rebuild`] with a
/// migration over the persisted rows. No handle is resident in this process,
/// so a corpus the daemon holds open falls back to its mtime for recency.
/// Blocking: call it from the blocking pool.
/// Test: `rebuild_moves_the_bad_file_aside_before_it_writes`.
pub fn rebuild_map(
    registry_path: &Path,
    runtime: Option<tokio::runtime::Handle>,
) -> Result<(Option<PathBuf>, ProjectMap), ProjectMapError> {
    let map_path = project_map_io::map_path_beside(registry_path);
    let persisted =
        crate::service::persistence::load_index_registry_at(registry_path).map_err(|e| {
            ProjectMapError::Registry {
                path: map_path.clone(),
                message: format!("{e:#}"),
            }
        })?;
    let ctx = MapContext {
        registry_path: registry_path.to_path_buf(),
        map_path: map_path.clone(),
        persisted,
        handles: Vec::new(),
    };
    let candidates = ctx.candidates();
    let disk = ctx.disk(runtime);
    project_map_io::quarantine_and_rebuild(&map_path, now_unix(), || {
        Ok(migrated(&candidates, &disk))
    })
}

/// Drop parked registrations from a prune's input (#9169: prune never deletes
/// a parked index).
///
/// # Errors
///
/// The map's own error when it cannot be read: a prune that cannot tell what
/// is parked deletes nothing.
pub fn retain_unparked(
    registry_path: &Path,
    entries: Vec<PersistedIndex>,
) -> anyhow::Result<Vec<PersistedIndex>> {
    let map_path = project_map_io::map_path_beside(registry_path);
    let parked = project_map_io::load(&map_path)?
        .map(|m| m.parked_ids())
        .unwrap_or_default();
    Ok(entries
        .into_iter()
        .filter(|e| !parked.contains(&e.id))
        .collect())
}

#[cfg(test)]
#[path = "project_map_state_tests.rs"]
mod tests;
