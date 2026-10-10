//! The boot side of `project-map.toml` (#9169 PR-B1).
//!
//! Why: the first boot after the upgrade writes the map (migration), and
//! every boot must leave parked indexes cold — no warm-load, no watcher, no
//! reconcile — and out of the boot orphan heal, which would deregister one.
//! What: [`BootMap::load`] runs [`super::project_map_state::ensure_map`] once
//! on the blocking pool; the rest filter the warm-boot entry list.
//! Test: `a_parked_index_is_skipped_on_boot` (`project_map_state_tests.rs`).

use std::collections::HashSet;

use crate::service::persistence::PersistedIndex;
use crate::service::project_map_state::{ensure_map, MapContext};
use crate::service::server::SearchAppState;

/// What a boot knows about the map.
#[derive(Debug, Clone, Default)]
pub struct BootMap {
    parked: HashSet<String>,
    readable: bool,
}

impl BootMap {
    /// Read the map, migrating it when absent.
    ///
    /// What: an unreadable `indexes.toml` or map is logged at error and gives
    /// an unreadable [`BootMap`]: nothing is parked (ruling: explicit ids keep
    /// working), nothing is migrated, and [`Self::heal`] heals nothing.
    /// Test: `a_parked_index_is_skipped_on_boot`.
    pub async fn load(state: &SearchAppState) -> Self {
        let ctx = match MapContext::capture(state) {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::error!("project-map: not loaded at boot, nothing migrated: {e}");
                return Self::default();
            }
        };
        let runtime = tokio::runtime::Handle::current();
        let loaded = tokio::task::spawn_blocking(move || {
            ensure_map(&ctx.map_path, &ctx.candidates(), &ctx.disk(Some(runtime)))
        })
        .await;
        match loaded {
            Ok(Ok(map)) => Self {
                parked: map.parked_ids(),
                readable: true,
            },
            Ok(Err(e)) => {
                tracing::error!("project-map: unreadable at boot: {e}");
                Self::default()
            }
            Err(e) => {
                tracing::error!("project-map: boot load task failed: {e}");
                Self::default()
            }
        }
    }

    /// A map read with these parked ids (tests).
    pub fn with_parked(parked: impl IntoIterator<Item = String>) -> Self {
        Self {
            parked: parked.into_iter().collect(),
            readable: true,
        }
    }

    /// Whether the map was read.
    pub fn is_readable(&self) -> bool {
        self.readable
    }

    /// `(parked, rest)` of `entries`.
    pub fn split(
        &self,
        entries: Vec<PersistedIndex>,
    ) -> (Vec<PersistedIndex>, Vec<PersistedIndex>) {
        entries
            .into_iter()
            .partition(|e| self.parked.contains(&e.id))
    }

    /// `entries` without the parked ones.
    pub fn without_parked(&self, entries: Vec<PersistedIndex>) -> Vec<PersistedIndex> {
        self.split(entries).1
    }

    /// The boot orphan heal, only when the map was read (#9169: a heal that
    /// cannot tell what is parked deregisters nothing).
    pub fn heal(&self, entries: Vec<PersistedIndex>) -> Vec<PersistedIndex> {
        if !self.readable {
            tracing::warn!("project-map unreadable: boot orphan heal skipped (#9169)");
            return entries;
        }
        crate::service::orphan_reaper::heal_boot_orphans(entries)
    }
}
