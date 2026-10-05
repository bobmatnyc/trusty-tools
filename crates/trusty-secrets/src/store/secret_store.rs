//! [`SecretStore`]: one backend plus the names-only index, as one unit.
//!
//! Why: a backend holds values; the index holds names and metadata. Every
//! operation must touch both in a fixed order, or `list` drifts from what is
//! stored. This type is that order, and the place reference resolution runs.
//! What: `set`/`list`/`delete`/`set_agents_may_use` for one vault, plus
//! `locate`/`read` for `secret://` references against a [`ScopeSet`].
//! A `set` writes the backend inside the index lock, after the index read, so
//! a corrupt or locked index fails before any value is stored and an index
//! row never claims a value the backend refused.
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
    /// `updated_at`; the agents flag is kept on update, OFF when new). A
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
    /// `store_set_reports_an_orphan_when_compensation_fails`.
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
            |_| self.backend.set(vault, key, value),
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
    /// What: reads the index only; never reads a value.
    /// Test: `store_list_reports_length_and_time_never_characters`.
    pub fn list(&self, vault: &VaultName) -> Result<Vec<KeyMeta>, SecretsError> {
        self.index.list(vault)
    }

    /// Remove `key` from `vault` (`secrets.delete`).
    ///
    /// What: deletes from the backend, then drops the index row. A refused
    /// backend delete leaves the row, so `list` never under-reports.
    /// `removed` is true when either held the key.
    /// Test: `store_delete_removes_entry_and_row`,
    /// `store_backend_errors_are_never_downgraded`.
    pub fn delete(
        &self,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<DeleteResponse, SecretsError> {
        self.require(Capabilities::WRITE, "delete")?;
        let existed = self.backend.delete(vault, key)?;
        let had_row = self.index.remove(vault, key)?;
        Ok(DeleteResponse {
            removed: existed || had_row,
        })
    }

    /// Set the "agents may use" flag on an indexed key (DOC-74 §15.8).
    ///
    /// Test: `index_upsert_preserves_the_agents_flag`.
    pub fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        self.index.set_agents_may_use(vault, key, allowed)
    }

    /// The vault a reference resolves to, by the names-only index.
    ///
    /// Why: DOC-74 §15.3 — `secret://KEY` checks the project vault, then the
    /// owner vault; explicit forms check only the vault they name. Resolution
    /// reads names, never values.
    /// What: the first vault whose index holds the key, or
    /// [`SecretsError::NotFound`] naming every vault searched.
    /// Test: `store_resolution_prefers_project_over_owner`,
    /// `store_resolution_miss_is_not_found`.
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
            Some(vault) => vec![vault],
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
        self.read_admitted(reference, scopes, |_, _| Ok(()))
    }

    /// [`Self::read`], with `admit` judging the located row before the
    /// backend is touched.
    ///
    /// Why: #7525 — a refused key must never reach the Keychain, so the gate
    /// runs between the index lookup and the value read.
    /// What: `require(READ)`, [`Self::locate_row`], `admit(vault, row)`, then
    /// the uncached backend read. An `admit` error is returned as-is.
    /// Test: `resolve_agent_gate_refuses_flag_off_before_any_read`.
    pub(crate) fn read_admitted(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
        admit: impl FnOnce(&VaultName, &KeyMeta) -> Result<(), SecretsError>,
    ) -> Result<SecretValue, SecretsError> {
        self.require(Capabilities::READ, "read")?;
        let (vault, row) = self.locate_row(reference, scopes)?;
        admit(&vault, &row)?;
        let key = reference.key();
        self.backend
            .get(&vault, key)?
            .ok_or_else(|| SecretsError::NotFound {
                key: key.to_string(),
                searched: vault.to_string(),
            })
    }
}
