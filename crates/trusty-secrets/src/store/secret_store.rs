//! [`SecretStore`]: one backend plus the names-only index, as one unit.
//!
//! Why: a backend holds values; the index holds names and metadata. Every
//! operation must touch both in a fixed order, or `list` drifts from what is
//! stored. This type is that order, and the place reference resolution runs.
//! What: `set`/`list`/`delete`/`set_agents_may_use` for one vault, plus
//! `locate`/`read` for `secret://` references against a [`ScopeSet`].
//! `delete_across` also clears backends other than the configured one (#7519).
//! A `set` writes the backend inside the index lock, after the index read, so
//! a corrupt or locked index fails before any value is stored and an index
//! row never claims a value the backend refused. #9070: the "agents may use"
//! flag is read from and written to the backend, never the index.
//! Test: `store_tests.rs` beside this file.

use std::fmt;
use std::sync::Arc;

use super::{Capabilities, NamesIndex, ScopeSet, SecretBackend, mask_secret, platform};
use crate::api::methods::{DeleteResponse, KeyMeta, SetOutcome, SetResponse};
use crate::api::{SecretKey, SecretRef, SecretValue, SecretsError, VaultName};

/// A backend and its names-only index.
///
/// Why: see the module docs.
/// What: cheap to clone the `Arc` it holds; holds no value itself. `Debug`
/// shows the backend id and the index root.
/// Test: `store_debug_never_contains_a_value`.
#[derive(Clone)]
pub struct SecretStore {
    backend: Arc<dyn SecretBackend>,
    index: NamesIndex,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretStore")
            .field("backend", &self.backend.id())
            .field("index_root", &self.index.root())
            .finish()
    }
}

impl SecretStore {
    /// Pair a backend with an index.
    pub fn new(backend: Arc<dyn SecretBackend>, index: NamesIndex) -> Self {
        Self { backend, index }
    }

    /// The backend.
    pub fn backend(&self) -> &dyn SecretBackend {
        self.backend.as_ref()
    }

    /// The index.
    pub fn index(&self) -> &NamesIndex {
        &self.index
    }

    fn require(
        &self,
        capability: Capabilities,
        operation: &'static str,
    ) -> Result<(), SecretsError> {
        if self.backend.capabilities().contains(capability) {
            Ok(())
        } else {
            Err(SecretsError::Unsupported {
                backend: self.backend.id().to_string(),
                operation,
            })
        }
    }

    /// Upsert `key` in `vault` (`secrets.set`).
    ///
    /// What: refuses an empty value and a backend without `WRITE`; then,
    /// under the index lock, writes the backend and the index row (length,
    /// `updated_at`). The backend's agents flag is kept on update; for a new
    /// key it is removed before the value is written, so a new key starts
    /// OFF and a failed removal stores nothing. A
    /// corrupt index or a lock timeout fails before the backend is touched; a
    /// backend failure leaves the index untouched. If the index publish fails
    /// after a new key reached the backend, the entry is deleted again while
    /// the lock is held; the caller gets the publish error, or
    /// [`SecretsError::OrphanedBackendEntry`] when that delete fails too.
    /// Returns the outcome and the one-time [`mask_secret`] confirmation.
    /// Test: `store_set_reports_outcome_and_mask_once`,
    /// `store_backend_errors_are_never_downgraded`,
    /// `store_set_fails_closed_on_the_index_before_the_backend_write`,
    /// `store_set_compensates_a_new_key_when_the_index_publish_fails`,
    /// `store_set_reports_an_orphan_when_compensation_fails`,
    /// `store_set_stores_no_value_when_the_flag_removal_fails`.
    pub fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<SetResponse, SecretsError> {
        if value.is_empty() {
            return Err(SecretsError::InvalidValue { reason: "is empty" });
        }
        self.require(Capabilities::WRITE, "write")?;
        // #9064: the backend write and its undo both run inside the index lock.
        let outcome = self.index.upsert_with(
            vault,
            key,
            value.char_len(),
            platform::now_unix(),
            |outcome| {
                // #9070: a stale flag item must not turn a new key ON.
                if outcome == SetOutcome::New {
                    self.backend.set_agents_may_use(vault, key, false)?;
                }
                self.backend.set(vault, key, value)
            },
            |outcome, publish| self.compensate(vault, key, outcome, publish),
        )?;
        Ok(SetResponse {
            outcome,
            masked: mask_secret(value.expose()),
        })
    }

    /// Undo a new key's backend write after the index publish failed.
    ///
    /// Why: #9064 — without the undo, the backend holds an entry no index row
    /// lists; a failed undo must be reported, not dropped.
    /// What: an updated key is left as written (the old value was never
    /// read, so it cannot be restored) and the publish error returned. A new
    /// key is deleted; on success the publish error is returned, on failure
    /// [`SecretsError::OrphanedBackendEntry`] carrying both errors.
    /// Test: `store_set_compensates_a_new_key_when_the_index_publish_fails`,
    /// `store_set_reports_an_orphan_when_compensation_fails`.
    fn compensate(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        outcome: SetOutcome,
        publish: SecretsError,
    ) -> SecretsError {
        if outcome != SetOutcome::New {
            return publish;
        }
        match self.backend.delete(vault, key) {
            Ok(_) => publish,
            Err(cleanup) => SecretsError::OrphanedBackendEntry {
                backend: self.backend.id().to_string(),
                vault: vault.to_string(),
                key: key.to_string(),
                source: Box::new(publish),
                cleanup: Box::new(cleanup),
            },
        }
    }

    /// Every key in `vault` with its metadata (`secrets.list`).
    ///
    /// What: names, lengths and times from the index; each key's agents flag
    /// from the backend (#9070), one flag lookup per key. A failed lookup
    /// fails the list; it never reports the flag ON. Never reads a value.
    /// Test: `store_list_reports_length_and_time_never_characters`,
    /// `store_list_and_get_ignore_a_hand_edited_index_flag`,
    /// `resolve_agent_gate_fails_closed_when_the_flag_read_fails`.
    pub fn list(&self, vault: &VaultName) -> Result<Vec<KeyMeta>, SecretsError> {
        let mut keys = self.index.list(vault)?;
        for meta in &mut keys {
            // #9070: the backend's flag item, never the index row's field.
            meta.agents_may_use = self.backend.agents_may_use(vault, &meta.name)?;
        }
        Ok(keys)
    }

    /// Remove `key` from `vault` in this store's backend and the index.
    ///
    /// What: [`Self::delete_across`] with no other backend. The server's
    /// `secrets.delete` uses `delete_across`, so a value left in another
    /// backend is cleared too (#7519).
    /// Test: `store_delete_removes_entry_and_row`,
    /// `store_backend_errors_are_never_downgraded`.
    pub fn delete(
        &self,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<DeleteResponse, SecretsError> {
        self.delete_across(vault, key, &[])
    }

    /// Remove `key` from `vault` in this store's backend, in each of
    /// `others`, and in the index (`secrets.delete`).
    ///
    /// Why: #7519 A5 — after a backend switch, or a `copy`, a value can sit
    /// in a backend other than the configured one. A delete that clears only
    /// the configured backend leaves that credential behind.
    /// What: refuses a configured backend without `WRITE`. Then, under the
    /// index lock (a corrupt or locked index fails before any backend is
    /// touched), deletes from the configured backend, then from each of
    /// `others`, and keeps going after a failure so every backend that can
    /// be cleared is. #9070: each backend's agents flag item is removed too,
    /// before its value, so a key set again later starts OFF. If any backend
    /// failed, the first error is returned and
    /// the index row is kept, so `list` still shows a key a backend may hold.
    /// Otherwise the row is dropped in the same locked update, so no `set`
    /// can land between the sweep and the removal. `removed` is true when
    /// any backend or the index held the key.
    /// Test: `store_delete_across_removes_the_key_from_every_backend`,
    /// `store_delete_across_keeps_the_row_when_any_backend_fails`,
    /// `store_delete_across_holds_the_index_lock_through_the_sweep`,
    /// `store_delete_keeps_the_row_when_the_flag_removal_fails`.
    pub fn delete_across(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        others: &[Arc<dyn SecretBackend>],
    ) -> Result<DeleteResponse, SecretsError> {
        self.require(Capabilities::WRITE, "delete")?;
        // #7519: the sweep and the row removal are one locked index update.
        let (existed, had_row) = self.index.remove_with(vault, key, || {
            let mut existed = false;
            let mut failure: Option<SecretsError> = None;
            for backend in std::iter::once(&self.backend).chain(others) {
                // #9070: the flag item goes with the key.
                let flag = backend.set_agents_may_use(vault, key, false);
                for result in [flag.map(|()| false), backend.delete(vault, key)] {
                    match result {
                        Ok(held) => existed |= held,
                        // #7519: a failed delete may leave a value; never a miss.
                        Err(e) if failure.is_none() => failure = Some(e),
                        Err(_) => {}
                    }
                }
            }
            failure.map_or(Ok(existed), Err)
        })?;
        Ok(DeleteResponse {
            removed: existed || had_row,
        })
    }

    /// Set the "agents may use" flag on an indexed key (DOC-74 §15.8).
    ///
    /// Why: #9070 — the flag in the index JSON was editable by any same-uid
    /// process; the backend's own flag item is not part of that file.
    /// What: under the index lock, a key with no row is
    /// [`SecretsError::NotFound`]; otherwise the backend creates (`true`) or
    /// removes (`false`) the key's flag item. A backend that holds no flag
    /// refuses `true` with [`SecretsError::Unsupported`]. The index file is
    /// not changed.
    /// Test: `store_agents_flag_survives_an_update_and_clears_on_delete`,
    /// `store_default_agent_flag_reads_off_and_refuses_on`.
    pub fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        // #9070: the backend holds the flag; the lock only orders it with delete.
        self.index.with_row(vault, key, || {
            self.backend.set_agents_may_use(vault, key, allowed)
        })
    }

    /// The vault a reference resolves to, by the names-only index.
    ///
    /// Why: DOC-74 §15.3 — `secret://KEY` checks the project vault, then the
    /// owner vault; explicit forms check only the vault they name, and that
    /// vault must be one of `scopes` (#9328). Resolution reads names, never
    /// values.
    /// What: the first vault whose index holds the key, or
    /// [`SecretsError::NotFound`] naming every vault searched. A pinned vault
    /// outside `scopes` is [`SecretsError::VaultOutOfScope`], before any read.
    /// Test: `store_resolution_prefers_project_over_owner`,
    /// `store_resolution_miss_is_not_found`,
    /// `resolve_pinned_reference_outside_the_scopes_is_refused`.
    pub fn locate(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
    ) -> Result<VaultName, SecretsError> {
        self.locate_row(reference, scopes).map(|(vault, _)| vault)
    }

    /// [`Self::locate`], also returning the index row that matched.
    ///
    /// Why: #7525 — the agents gate must judge the same row the read uses,
    /// from one index read, so the row cannot change between check and read.
    /// Test: `resolve_agent_gate_refuses_flag_off_before_any_read`.
    fn locate_row(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
    ) -> Result<(VaultName, KeyMeta), SecretsError> {
        let key = reference.key();
        let candidates: Vec<VaultName> = match reference.pinned_vault() {
            // #9328: ruling 06 R1 — a pinned vault must be one of the
            // caller's own, refused before the index is read.
            Some(vault) => {
                scopes.require_in_scope(&vault)?;
                vec![vault]
            }
            None => scopes.lookup_order().cloned().collect(),
        };
        for vault in &candidates {
            if let Some(row) = self.index.get(vault, key)? {
                return Ok((vault.clone(), row));
            }
        }
        Err(SecretsError::NotFound {
            key: key.to_string(),
            searched: candidates
                .iter()
                .map(VaultName::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        })
    }

    /// Read the value a reference names.
    ///
    /// What: refuses a backend without `READ` (a sync target), locates the
    /// vault, and reads the backend directly — no cache (DOC-74 §15.5). An
    /// indexed key the backend no longer holds is `NotFound`; a backend
    /// failure stays a backend error.
    /// Test: `store_resolution_prefers_project_over_owner`,
    /// `store_backend_errors_are_never_downgraded`,
    /// `store_capabilities_gate_operations`.
    pub fn read(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
    ) -> Result<SecretValue, SecretsError> {
        self.require(Capabilities::READ, "read")?;
        let (vault, _) = self.locate_row(reference, scopes)?;
        self.read_located(&vault, reference.key())
    }

    /// [`Self::read`], with `admit` judging the located row before the
    /// backend is touched.
    ///
    /// Why: #7525 — a refused key must never reach the Keychain, so the gate
    /// runs between the index lookup and the value read.
    /// What: `require(READ)`, [`Self::locate_row`], the row's agents flag
    /// from the backend (#9070), `admit(vault, row)`, then the uncached
    /// backend read. A failed flag lookup and an `admit` error are returned
    /// as-is, before the value is read.
    /// Test: `resolve_agent_gate_refuses_flag_off_before_any_read`,
    /// `resolve_agent_gate_ignores_a_hand_edited_index_flag`,
    /// `resolve_agent_gate_fails_closed_when_the_flag_read_fails`.
    pub(crate) fn read_admitted(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
        admit: impl FnOnce(&VaultName, &KeyMeta) -> Result<(), SecretsError>,
    ) -> Result<SecretValue, SecretsError> {
        self.require(Capabilities::READ, "read")?;
        let (vault, mut row) = self.locate_row(reference, scopes)?;
        // #9070: the gate judges the backend's flag item, never the index row.
        row.agents_may_use = self.backend.agents_may_use(&vault, &row.name)?;
        admit(&vault, &row)?;
        self.read_located(&vault, reference.key())
    }

    /// The uncached backend read of a located key; a miss is `NotFound`.
    fn read_located(
        &self,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<SecretValue, SecretsError> {
        self.backend
            .get(vault, key)?
            .ok_or_else(|| SecretsError::NotFound {
                key: key.to_string(),
                searched: vault.to_string(),
            })
    }
}
