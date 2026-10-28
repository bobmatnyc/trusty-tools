//! The per-key agent deny and its one-time vault migration (#10386).
//!
//! Why: owner ruling 2026-10-28 (Bob D69/Af9, D6a) — tm secrets exists for
//! agents, and the model is `.env.local`'s: a key in the caller's scope is
//! usable by an agent unless the operator denied it. The pre-#10386 flag was
//! an opt-in ("agents may use"), stored as its own backend item (#9070).
//! What: [`ControlItem`] names the backend items this rule reads: a key's
//! deny, the legacy opt-in flag a migration reads once, and the per-vault
//! migration marker. [`AgentDenyMigration`] says how a vault that predates
//! the deny is migrated, and [`DEFAULT_AGENT_DENY_MIGRATION`] is the one
//! switch. [`migrate_vault`] runs the migration; [`agent_denied`] is the
//! lookup every agent gate uses. A failed lookup, a failed migration step and
//! an unreadable marker are errors, never "allowed" (Fail-Open Check).
//! Test: `agent_deny_tests.rs` beside this file.

use std::sync::Arc;

use super::{NamesIndex, SecretBackend};
use crate::api::{SecretKey, SecretsError, VaultName};

/// Called before a deny is written (`true`) or removed (`false`) outside
/// `secrets.set_agents_may_use`, which records its own change (#10386).
///
/// Why: Architect acceptance criterion (2026-10-28) — every deny change has
/// a write-ahead audit record. The store has no audit log; the server gives
/// it this hook.
/// What: an `Err` stops the change before the backend is touched; the
/// server's hook answers [`SecretsError::AuditUnavailable`]. It is given the
/// vault, the key and the deny state after the change, never a value.
pub type DenyAudit =
    Arc<dyn Fn(&VaultName, &SecretKey, bool) -> Result<(), SecretsError> + Send + Sync>;

/// Write (`denied`) or remove a key's deny, after `audit` records it.
///
/// What: `audit` first, then the backend write; an audit error leaves the
/// backend untouched. `None` writes with no record (a store with no log).
/// Test: `migration_keep_closed_records_each_deny_before_writing_it`.
pub fn change_deny(
    backend: &dyn SecretBackend,
    vault: &VaultName,
    key: &SecretKey,
    denied: bool,
    audit: Option<&DenyAudit>,
) -> Result<(), SecretsError> {
    if let Some(record) = audit {
        record(vault, key, denied)?;
    }
    backend.set_control_item(ControlItem::AgentDeny, vault, Some(key), denied)
}

/// One agent-control item a backend may hold beside a key's value.
///
/// What: each is present or absent; its stored text is never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlItem {
    /// The key is denied to callers under a Claude Code parent. Per key.
    AgentDeny,
    /// The pre-#10386 "agents may use" opt-in (#9070). Per key; read only by
    /// [`migrate_vault`], then removed.
    LegacyAgentsFlag,
    /// The vault was migrated to the deny model. Per vault (no key).
    DenyMigrated,
}

impl ControlItem {
    /// The operation name an [`SecretsError::Unsupported`] carries.
    pub fn operation(self) -> &'static str {
        match self {
            Self::AgentDeny => "agent_deny",
            Self::LegacyAgentsFlag => "agents_may_use",
            Self::DenyMigrated => "agent_deny_migration",
        }
    }
}

/// How a vault written before #10386 moves to the deny model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDenyMigration {
    /// Every indexed key becomes agent-usable; no deny item is written.
    OpenAll,
    /// A key whose legacy flag was off gets a deny item; a flagged key
    /// stays usable. Existing keys keep their pre-#10386 behaviour.
    KeepClosed,
}

impl AgentDenyMigration {
    /// The marker text naming this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAll => "open-all",
            Self::KeepClosed => "keep-closed",
        }
    }
}

/// The migration every store runs unless built with another mode.
// #10386: Bob D6a (2026-10-28, via the Architect) — "Security model is
// exactly like .env.local. If it's in the scope (user/owner/project), then
// it's usable by the agent." Flip to `KeepClosed` here to keep old keys shut.
pub const DEFAULT_AGENT_DENY_MIGRATION: AgentDenyMigration = AgentDenyMigration::OpenAll;

/// Migrate `vault` on `backend` to the deny model, once.
///
/// Why: #10386 Q1 — the meaning of a stored item flipped, so a vault written
/// under the opt-in flag must be translated before any deny is read.
/// What: under the vault's index lock. A backend that holds no control item
/// has nothing to migrate. A vault whose marker is present is left alone
/// (idempotent). Otherwise, for each indexed key: [`AgentDenyMigration::KeepClosed`]
/// writes a deny for each key whose legacy flag is absent; then every legacy
/// flag is removed; then the marker is written. A crash part-way leaves no
/// marker, so the rerun repeats it; a key whose legacy flag was already
/// removed then reads as unflagged and is denied under `KeepClosed`, which
/// fails closed. Every backend error is returned as-is. Each deny goes
/// through [`change_deny`], so `audit` records it first.
/// Test: `migration_open_all_writes_no_deny_and_clears_legacy`,
/// `migration_keep_closed_records_each_deny_before_writing_it`,
/// `migration_keep_closed_denies_unflagged_and_opens_flagged`,
/// `migration_rerun_is_a_no_op`, `migration_unreadable_marker_refuses`.
pub fn migrate_vault(
    backend: &dyn SecretBackend,
    index: &NamesIndex,
    vault: &VaultName,
    mode: AgentDenyMigration,
    audit: Option<&DenyAudit>,
) -> Result<(), SecretsError> {
    if !backend.holds_agent_controls() {
        return Ok(());
    }
    index.with_names(vault, |names| {
        if backend.control_item(ControlItem::DenyMigrated, vault, None)? {
            return Ok(());
        }
        let mut flagged = Vec::with_capacity(names.len());
        for key in &names {
            flagged.push(backend.control_item(ControlItem::LegacyAgentsFlag, vault, Some(key))?);
        }
        if mode == AgentDenyMigration::KeepClosed {
            for (key, on) in names.iter().zip(&flagged) {
                if !on {
                    change_deny(backend, vault, key, true, audit)?;
                }
            }
        }
        for (key, on) in names.iter().zip(&flagged) {
            if *on {
                backend.set_control_item(ControlItem::LegacyAgentsFlag, vault, Some(key), false)?;
            }
        }
        backend.set_control_item(ControlItem::DenyMigrated, vault, None, true)
    })
}

/// Whether `key` in `vault` is denied to agents, migrating the vault first.
///
/// What: a backend that holds no control item cannot record a deny, so its
/// keys follow `mode`'s default: open under [`AgentDenyMigration::OpenAll`],
/// denied under [`AgentDenyMigration::KeepClosed`]. Otherwise
/// [`migrate_vault`], then the deny item's presence. Any error is returned,
/// never read as "not denied".
/// Test: `agent_denied_fails_closed_when_the_lookup_fails`,
/// `agent_denied_without_controls_follows_the_mode`.
pub fn agent_denied(
    backend: &dyn SecretBackend,
    index: &NamesIndex,
    vault: &VaultName,
    key: &SecretKey,
    mode: AgentDenyMigration,
    audit: Option<&DenyAudit>,
) -> Result<bool, SecretsError> {
    if !backend.holds_agent_controls() {
        return Ok(mode == AgentDenyMigration::KeepClosed);
    }
    migrate_vault(backend, index, vault, mode, audit)?;
    backend.control_item(ControlItem::AgentDeny, vault, Some(key))
}

#[cfg(test)]
#[path = "agent_deny_tests.rs"]
mod tests;
