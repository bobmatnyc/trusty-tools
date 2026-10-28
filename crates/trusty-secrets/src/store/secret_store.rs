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
//! row never claims a value the backend refused. #9070: the agent rule is
//! read from and written to the backend, never the index. #10386: the stored
//! item is a deny; a key with none is usable by agents, and a vault written
//! before #10386 is migrated once ([`super::agent_deny`]). #7520: a
//! store given a [`SecretCache`] keeps the CLI-backed values it reads, after
//! every gate on the read path has run.
//! Test: `store_tests.rs` beside this file.

use std::fmt;
use std::sync::Arc;

use super::agent_deny::{
    self, AgentDenyMigration, ControlItem, DEFAULT_AGENT_DENY_MIGRATION, DenyAudit, change_deny,
};
use super::{
    Capabilities, NamesIndex, OpenedUnder, ScopeSet, SecretBackend, SecretCache, mask_secret,
    platform,
};
use crate::api::methods::{DeleteResponse, KeyMeta, SetOutcome, SetResponse};
use crate::api::{SecretKey, SecretRef, SecretValue, SecretsError, VaultName};

/// A backend and its names-only index.
///
/// Why: see the module docs.
/// What: cheap to clone the `Arc`s it holds; holds no value itself, only
/// the optional session cache does (#7520). `Debug` shows the backend id,
/// the index root and the cache's entry count.
/// Test: `store_debug_never_contains_a_value`, `cache_debug_never_prints_value`.
#[derive(Clone)]
pub struct SecretStore {
    backend: Arc<dyn SecretBackend>,
    index: NamesIndex,
    // #7520: the server's session cache; `None` reads the backend every time.
    cache: Option<Arc<SecretCache>>,
    // #7520 M1: the settings `backend` was opened under; `None` is unknown,
    // and the cache is then bypassed. Kept out of `Debug`.
    opened_under: Option<OpenedUnder>,
    // #10386: how a vault written before the deny model is migrated.
    migration: AgentDenyMigration,
    // #10386: records each deny change the store makes on its own.
    deny_audit: Option<DenyAudit>,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretStore")
            .field("backend", &self.backend.id())
            .field("index_root", &self.index.root())
            .field("cache", &self.cache)
            .finish()
    }
}

impl SecretStore {
    /// Pair a backend with an index.
    pub fn new(backend: Arc<dyn SecretBackend>, index: NamesIndex) -> Self {
        Self {
            backend,
            index,
            cache: None,
            opened_under: None,
            migration: DEFAULT_AGENT_DENY_MIGRATION,
            deny_audit: None,
        }
    }

    /// This store, recording every deny it writes or removes through `audit`
    /// first (#10386): the migration's denies, a deny `delete` removes, and
    /// a deny a copy carries.
    ///
    /// Test: `migration_keep_closed_records_each_deny_before_writing_it`,
    /// `store_delete_records_the_deny_it_removes`.
    pub fn with_deny_audit(mut self, audit: DenyAudit) -> Self {
        self.deny_audit = Some(audit);
        self
    }

    /// This store, migrating pre-#10386 vaults with `mode` instead of
    /// [`DEFAULT_AGENT_DENY_MIGRATION`].
    ///
    /// Test: `migration_keep_closed_denies_unflagged_and_opens_flagged`.
    pub fn with_agent_deny_migration(mut self, mode: AgentDenyMigration) -> Self {
        self.migration = mode;
        self
    }

    /// Migrate `vault` to the deny model if it was not yet ([`agent_deny::migrate_vault`]).
    fn migrate(&self, vault: &VaultName) -> Result<(), SecretsError> {
        agent_deny::migrate_vault(
            self.backend.as_ref(),
            &self.index,
            vault,
            self.migration,
            self.deny_audit.as_ref(),
        )
    }

    /// Whether agents may use `key` in `vault`: no deny item (#10386).
    ///
    /// What: the inverse of [`agent_deny::agent_denied`]. A failed lookup or
    /// migration is an error, never "may use".
    /// Test: `agent_denied_fails_closed_when_the_lookup_fails`.
    pub fn agents_may_use(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        agent_deny::agent_denied(
            self.backend.as_ref(),
            &self.index,
            vault,
            key,
            self.migration,
            self.deny_audit.as_ref(),
        )
        .map(|denied| !denied)
    }

    /// [`Self::agents_may_use`] for a vault already migrated.
    fn may_use_migrated(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        if !self.backend.holds_agent_controls() {
            return Ok(self.migration == AgentDenyMigration::OpenAll);
        }
        self.backend
            .control_item(ControlItem::AgentDeny, vault, Some(key))
            .map(|denied| !denied)
    }

    /// This store, reading CLI-backed values through `cache` (#7520), as
    /// filled under `opened_under`, the settings the backend was opened under.
    ///
    /// What: only the read path consults it, after its gates; writes never
    /// touch it (the server evicts). `opened_under: None` bypasses it.
    /// Test: `resolve_second_call_does_not_reread_cli_backend`,
    /// `cache_cleared_when_cli_account_settings_change`.
    #[cfg_attr(
        not(feature = "server"),
        allow(dead_code, reason = "only the server's resolve gives a cache")
    )]
    pub(crate) fn with_cache(
        mut self,
        cache: Arc<SecretCache>,
        opened_under: Option<OpenedUnder>,
    ) -> Self {
        self.cache = Some(cache);
        self.opened_under = opened_under;
        self
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
    /// `updated_at`). #10386: the vault is migrated to the deny model first.
    /// `set` never touches a deny: an update keeps it, and a deny left from
    /// an earlier key of the same name stays (fail closed). A
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
    /// `store_set_keeps_a_stale_deny_on_a_new_key`.
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
        // #10386: before the index lock, which the migration takes itself.
        self.migrate(vault)?;
        // #9064: the backend write and its undo both run inside the index lock.
        let outcome = self.index.upsert_with(
            vault,
            key,
            value.char_len(),
            platform::now_unix(),
            // #10386: no deny is cleared here; a stale one keeps the key shut.
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
    /// What: names, lengths and times from the index; `agents_may_use` is the
    /// absence of the key's deny item in the backend (#9070, #10386), after
    /// the vault's migration, one lookup per key. A failed lookup fails the
    /// list; it never reports a key usable. Never reads a value.
    /// Test: `store_list_reports_length_and_time_never_characters`,
    /// `store_list_and_get_ignore_a_hand_edited_index_flag`,
    /// `resolve_agent_gate_fails_closed_when_the_deny_read_fails`.
    pub fn list(&self, vault: &VaultName) -> Result<Vec<KeyMeta>, SecretsError> {
        if self.backend.holds_agent_controls() {
            self.migrate(vault)?;
        }
        let mut keys = self.index.list(vault)?;
        for meta in &mut keys {
            // #9070: the backend's item, never the index row's field.
            meta.agents_may_use = self.may_use_migrated(vault, &meta.name)?;
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
    /// be cleared is. #10386: only when every value delete succeeded are the
    /// key's control items removed from each backend that holds them (a deny
    /// through the audit hook first), so a failed delete leaves the deny in
    /// place. If any backend failed, the first error is returned and
    /// the index row is kept, so `list` still shows a key a backend may hold.
    /// Otherwise the row is dropped in the same locked update, so no `set`
    /// can land between the sweep and the removal. `removed` is true when
    /// any backend or the index held the key.
    /// Test: `store_delete_across_removes_the_key_from_every_backend`,
    /// `store_delete_across_keeps_the_row_when_any_backend_fails`,
    /// `store_delete_across_holds_the_index_lock_through_the_sweep`,
    /// `store_delete_keeps_the_row_when_the_flag_removal_fails`,
    /// `store_delete_keeps_the_deny_when_a_value_delete_fails`,
    /// `store_delete_records_the_deny_it_removes`.
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
            let backends = || std::iter::once(&self.backend).chain(others);
            for backend in backends() {
                match backend.delete(vault, key) {
                    Ok(held) => existed |= held,
                    // #7519: a failed delete may leave a value; never a miss.
                    Err(e) if failure.is_none() => failure = Some(e),
                    Err(_) => {}
                }
            }
            if let Some(e) = failure {
                return Err(e);
            }
            // #10386: the deny goes only once every value is gone.
            for backend in backends().filter(|b| b.holds_agent_controls()) {
                self.clear_controls(backend.as_ref(), vault, key)?;
            }
            Ok(existed)
        })?;
        Ok(DeleteResponse {
            removed: existed || had_row,
        })
    }

    /// Remove `key`'s deny (recorded first, when present) and legacy flag
    /// from `backend` (#10386).
    fn clear_controls(
        &self,
        backend: &dyn SecretBackend,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<(), SecretsError> {
        if backend.control_item(ControlItem::AgentDeny, vault, Some(key))? {
            change_deny(backend, vault, key, false, self.deny_audit.as_ref())?;
        }
        backend.set_control_item(ControlItem::LegacyAgentsFlag, vault, Some(key), false)
    }

    /// Before a copy writes `key` here from `source`, carry its deny (#10386).
    ///
    /// Why: code-critic BLOCK on 90558c5cbc — a denied key copied into a
    /// backend with no deny read as open, an escape from the operator's deny.
    /// What: a key `source` does not deny needs nothing. A denied key is
    /// [`SecretsError::Unsupported`] when this backend cannot hold a deny;
    /// otherwise this vault is migrated and the deny written here (recorded
    /// first), before the caller writes the value.
    /// Test: `server_copy_of_a_denied_key_into_a_no_controls_backend_is_refused`,
    /// `server_copy_of_a_denied_key_carries_the_deny`.
    pub(crate) fn carry_deny_from(
        &self,
        source: &SecretStore,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<(), SecretsError> {
        if source.agents_may_use(vault, key)? {
            return Ok(());
        }
        if !self.backend.holds_agent_controls() {
            return Err(SecretsError::Unsupported {
                backend: self.backend.id().to_string(),
                operation: ControlItem::AgentDeny.operation(),
            });
        }
        self.migrate(vault)?;
        change_deny(
            self.backend.as_ref(),
            vault,
            key,
            true,
            self.deny_audit.as_ref(),
        )
    }

    /// Allow (`true`) or deny (`false`) agents an indexed key (DOC-74 §15.8).
    ///
    /// Why: #9070 — the rule in the index JSON was editable by any same-uid
    /// process; the backend's own item is not part of that file. #10386: the
    /// item is a deny, so `allowed` removes it and `!allowed` writes it.
    /// What: the vault is migrated first; then, under the index lock, a key
    /// with no row is [`SecretsError::NotFound`]; otherwise the backend
    /// removes (`true`) or creates (`false`) the key's deny item. A backend
    /// that holds no control item refuses a deny with
    /// [`SecretsError::Unsupported`], and refuses `true` unless its keys are
    /// already open ([`AgentDenyMigration::OpenAll`]). The index file is not
    /// changed.
    /// Test: `store_agent_deny_survives_an_update_and_clears_on_delete`,
    /// `store_default_backend_holds_no_agent_controls`.
    pub fn set_agents_may_use(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        self.set_agents_may_use_after(vault, key, allowed, || Ok::<(), SecretsError>(()))?
    }

    /// The backend write behind [`Self::set_agents_may_use`] (#10386).
    fn write_deny(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
    ) -> Result<(), SecretsError> {
        if !self.backend.holds_agent_controls() {
            // #10386: nothing to remove where every key is already open.
            return if allowed && self.migration == AgentDenyMigration::OpenAll {
                Ok(())
            } else {
                Err(SecretsError::Unsupported {
                    backend: self.backend.id().to_string(),
                    operation: ControlItem::AgentDeny.operation(),
                })
            };
        }
        self.backend
            .set_control_item(ControlItem::AgentDeny, vault, Some(key), !allowed)
    }

    /// [`Self::set_agents_may_use`], running `before` under the same index
    /// lock, after the row check and before the backend call.
    ///
    /// Why: #9621 — the server's write-ahead audit record goes in `before`,
    /// so records and flag changes for one key land in the same order, and
    /// a key with no row is refused before any allow record is written.
    /// What: `Ok(Err(e))` when `before` refused (the backend is never
    /// called); `Ok(Ok(()))` when the flag was written; `Err` for a missing
    /// row, a lock or index failure, or a backend failure.
    /// Test: `store_flag_before_hook_runs_under_the_row_check`.
    pub(crate) fn set_agents_may_use_after<E>(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        allowed: bool,
        before: impl FnOnce() -> Result<(), E>,
    ) -> Result<Result<(), E>, SecretsError> {
        // #10386: before the index lock, which the migration takes itself.
        if self.backend.holds_agent_controls() {
            self.migrate(vault)?;
        }
        // #9070: the backend holds the item; the lock only orders it with delete.
        self.index.with_row(vault, key, || match before() {
            Ok(()) => self.write_deny(vault, key, allowed).map(Ok),
            Err(refused) => Ok(Err(refused)),
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
    /// Test: `resolve_agent_gate_refuses_a_denied_key_before_any_read`.
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
    /// vault, and reads it through [`Self::read_located`] (DOC-74 §15.5). An
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
    /// from the backend (#9070), `admit(vault, row)`, then
    /// [`Self::read_located`]. A failed flag lookup and an `admit` error are returned
    /// as-is, before the value is read.
    /// Test: `resolve_agent_gate_refuses_a_denied_key_before_any_read`,
    /// `resolve_agent_gate_ignores_a_hand_edited_index_flag`,
    /// `resolve_agent_gate_fails_closed_when_the_deny_read_fails`.
    pub(crate) fn read_admitted(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
        admit: impl FnOnce(&VaultName, &KeyMeta) -> Result<(), SecretsError>,
    ) -> Result<SecretValue, SecretsError> {
        self.require(Capabilities::READ, "read")?;
        let (vault, row) = self.locate_flagged(reference, scopes)?;
        admit(&vault, &row)?;
        self.read_located(&vault, reference.key())
    }

    /// [`Self::locate_row`], with the row's agents flag from the backend.
    ///
    /// Why: #9070 — `secrets.grant` judges the flag of every key it grants
    /// to an agent-parented registrar without reading a value, through the
    /// same lookup the read path's gate uses.
    /// What: the located vault and row; `agents_may_use` is the absence of
    /// the backend's deny item after the vault's migration (#10386), never
    /// the index row's field. A failed lookup is an error, never "may use".
    /// No value is read.
    /// Test: `grant_by_agent_ancestor_cannot_name_denied_key`,
    /// `resolve_agent_gate_fails_closed_when_the_deny_read_fails`.
    pub(crate) fn locate_flagged(
        &self,
        reference: &SecretRef,
        scopes: &ScopeSet,
    ) -> Result<(VaultName, KeyMeta), SecretsError> {
        let (vault, mut row) = self.locate_row(reference, scopes)?;
        // #9070: the gate judges the backend's item, never the index row.
        row.agents_may_use = self.agents_may_use(&vault, &row.name)?;
        Ok((vault, row))
    }

    /// The backend read of a located key; a miss is `NotFound`.
    ///
    /// What: with a session cache, through [`SecretCache::read_through`], so
    /// a CLI-backed value already read in this session is not read again;
    /// otherwise straight from the backend. Callers run every gate first.
    /// Test: `resolve_second_call_does_not_reread_cli_backend`,
    /// `warm_cache_still_refuses_denied_key_for_agent`.
    fn read_located(
        &self,
        vault: &VaultName,
        key: &SecretKey,
    ) -> Result<SecretValue, SecretsError> {
        // #7520: after `admit` and the flag gate; a hit skips only this read.
        let value = match &self.cache {
            Some(cache) => cache.read_through(
                &self.backend.id(),
                self.opened_under.as_ref(),
                vault,
                key,
                || self.backend.get(vault, key),
            )?,
            None => self.backend.get(vault, key)?,
        };
        value.ok_or_else(|| SecretsError::NotFound {
            key: key.to_string(),
            searched: vault.to_string(),
        })
    }
}
