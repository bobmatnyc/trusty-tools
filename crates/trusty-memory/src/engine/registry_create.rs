//! [`PalaceRegistry::create_palace`] and [`PalaceRegistry::create_palace_or_existing`].
//!
//! Why (#11823): the create call wrote `palace.json` with no existence check,
//! so a second create for an existing id replaced its metadata. Every caller —
//! MCP, the service, `force=true`, launch-time ensure — goes through here, so
//! the guard lives here.
//! What: the create pipeline plus [`PalaceCreateOutcome`], which tells a caller
//! whether it created the palace, found it, or found a live alias.
//! Test: `create_on_an_existing_palace_leaves_palace_json_byte_identical`,
//! `registry_create_and_open`, `palace_create_refuses_a_live_alias_name`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::PalaceRegistry;
use crate::engine::palace::Palace;
use crate::engine::retrieval::PalaceHandle;
use crate::engine::store::concurrent_open::OpenIntent;
use crate::engine::store::palace_store::PalaceStore;

/// What a create request found (#11823).
#[non_exhaustive]
pub enum PalaceCreateOutcome {
    /// The palace was new: `palace.json` written, handle opened.
    Created(Arc<PalaceHandle>),
    /// A palace with this id already existed; its `palace.json` was not
    /// touched and this is its handle.
    Exists(Arc<PalaceHandle>),
    /// The id is a live alias of `target` (#9544); nothing was created.
    LiveAlias {
        /// The palace the alias resolves to.
        target: String,
    },
}

// `PalaceHandle` has no `Debug`; name the variant and the id.
impl std::fmt::Debug for PalaceCreateOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created(h) => write!(f, "Created({})", h.id),
            Self::Exists(h) => write!(f, "Exists({})", h.id),
            Self::LiveAlias { target } => write!(f, "LiveAlias {{ target: {target:?} }}"),
        }
    }
}

impl PalaceRegistry {
    /// Create and persist a new palace, then open it.
    ///
    /// Why: `palace new` saves metadata and immediately wants a working handle
    /// for further operations; combining the steps avoids a TOCTOU between
    /// save and open.
    /// What: [`Self::create_palace_or_existing`], returning the handle whether
    /// the palace was created or already existed (its metadata is never
    /// rewritten, #11823). A live alias id is refused with
    /// [`trusty_common::palace_alias::LiveAliasError`] (#9544).
    /// Test: `registry_create_and_open`, `palace_create_refuses_a_live_alias_name`,
    /// `palace_create_fails_when_the_alias_file_is_unreadable`,
    /// `create_on_an_existing_palace_leaves_palace_json_byte_identical`.
    pub fn create_palace(&self, data_root: &Path, palace: Palace) -> Result<Arc<PalaceHandle>> {
        let alias = palace.id.as_str().to_string();
        match self.create_palace_or_existing(data_root, palace)? {
            PalaceCreateOutcome::Created(handle) | PalaceCreateOutcome::Exists(handle) => {
                Ok(handle)
            }
            PalaceCreateOutcome::LiveAlias { target } => {
                Err(trusty_common::palace_alias::LiveAliasError { alias, target }.into())
            }
        }
    }

    /// Create a palace unless one, or a live alias, already holds the id.
    ///
    /// Why (#11823): an existing palace's `palace.json` must never be
    /// overwritten by a create, and a caller ensuring a palace exists needs to
    /// know which case it hit.
    /// What: an observer registry refuses (#9659). Existing `palace.json`
    /// (`try_exists`; a probe error refuses the create) → open it and return
    /// [`PalaceCreateOutcome::Exists`] without writing. A live alias →
    /// [`PalaceCreateOutcome::LiveAlias`]; an unreadable alias file refuses
    /// (#9544). Otherwise creates `data_root/<id>`, writes `palace.json`, opens
    /// and registers the handle → [`PalaceCreateOutcome::Created`].
    /// Test: `create_on_an_existing_palace_leaves_palace_json_byte_identical`,
    /// `palace_create_fails_when_the_alias_file_is_unreadable`.
    pub fn create_palace_or_existing(
        &self,
        data_root: &Path,
        mut palace: Palace,
    ) -> Result<PalaceCreateOutcome> {
        // #9659: an observer registry never creates a palace.
        anyhow::ensure!(
            self.open_intent != OpenIntent::Observer,
            "palace {} does not exist; a read-only observer never creates one",
            palace.id
        );
        // Always anchor data_dir under data_root/<id> so callers can pass a
        // bare Palace without worrying about path layout.
        let palace_dir = data_root.join(palace.id.as_str());
        // #11823: an existing palace is opened, never re-saved. A probe that
        // cannot answer refuses rather than presuming absent.
        let present = palace_dir
            .join("palace.json")
            .try_exists()
            .with_context(|| format!("check whether palace {} exists", palace.id))?;
        if present {
            return Ok(PalaceCreateOutcome::Exists(
                self.open_palace(data_root, &palace.id)?,
            ));
        }
        // #9544: a live alias name stays an alias; creating it would shadow it.
        // An unreadable alias file fails the create rather than skipping the check.
        if let Some(target) =
            trusty_common::palace_alias::try_alias_target_if_absent(data_root, palace.id.as_str())
                .with_context(|| format!("check palace id {} against the alias map", palace.id))?
        {
            return Ok(PalaceCreateOutcome::LiveAlias { target });
        }
        palace.data_dir = palace_dir.clone();
        std::fs::create_dir_all(&palace_dir)
            .with_context(|| format!("create palace dir {}", palace_dir.display()))?;
        PalaceStore::save_palace(&palace)
            .with_context(|| format!("save palace metadata for {}", palace.id))?;
        // Issue #1487: honour the registry's open intent (Writer on the HTTP
        // daemon) so a freshly-created palace is opened under the same
        // fail-loud contract as a re-opened one.
        let handle = self.open_handle(&palace)?;
        // ADR-0027 T2: name the rooms this palace's drawers already sit in.
        Self::backfill_rooms(&handle);
        self.register_arc(handle.clone());
        Ok(PalaceCreateOutcome::Created(handle))
    }
}
