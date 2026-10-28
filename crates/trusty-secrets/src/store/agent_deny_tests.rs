//! Tests for the agent deny and its vault migration (#10386).
//!
//! Every test runs against [`MemoryBackend`] and a temp-dir index. A
//! "pre-#10386 vault" is seeded through the index and the backend directly,
//! so no migration has run on it yet.
//!
//! Test: itself.

use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::store::{MemoryBackend, SecretStore};

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

/// A pre-#10386 vault: `OFF_KEY` unflagged, `ON_KEY` with the legacy flag.
fn legacy_vault() -> (TempDir, Arc<MemoryBackend>, NamesIndex) {
    let tmp = TempDir::new().unwrap();
    let index = NamesIndex::at(tmp.path().join("index"));
    let backend = Arc::new(MemoryBackend::new());
    for name in ["OFF_KEY", "ON_KEY"] {
        index.upsert(&vault(), &key(name), 10, 1).unwrap();
    }
    backend
        .set_control_item(
            ControlItem::LegacyAgentsFlag,
            &vault(),
            Some(&key("ON_KEY")),
            true,
        )
        .unwrap();
    (tmp, backend, index)
}

fn item(backend: &MemoryBackend, item: ControlItem, name: Option<&str>) -> bool {
    let key = name.map(key);
    backend.control_item(item, &vault(), key.as_ref()).unwrap()
}

/// Why: Bob D6a (2026-10-28) — the `.env.local` model: an in-scope key is
/// usable by agents. Red when the default switch is flipped back.
/// Test: itself.
#[test]
fn default_migration_is_open_all() {
    assert_eq!(DEFAULT_AGENT_DENY_MIGRATION, AgentDenyMigration::OpenAll);
    let tmp = TempDir::new().unwrap();
    let store = SecretStore::new(
        Arc::new(MemoryBackend::new()),
        NamesIndex::at(tmp.path().join("index")),
    );
    assert!(store.agents_may_use(&vault(), &key("ANY")).unwrap());
}

/// Why: Q1 open-all — no deny is written from an old OFF flag, and the
/// legacy items go. Red when open-all writes a deny or leaves a legacy flag.
/// Test: itself.
#[test]
fn migration_open_all_writes_no_deny_and_clears_legacy() {
    let (_tmp, backend, index) = legacy_vault();
    migrate_vault(
        backend.as_ref(),
        &index,
        &vault(),
        AgentDenyMigration::OpenAll,
    )
    .unwrap();
    for name in ["OFF_KEY", "ON_KEY"] {
        assert!(
            !item(&backend, ControlItem::AgentDeny, Some(name)),
            "{name} denied"
        );
        assert!(!item(&backend, ControlItem::LegacyAgentsFlag, Some(name)));
    }
    assert!(item(&backend, ControlItem::DenyMigrated, None));
}

/// Why: Q1 keep-closed, the alternate mode — existing keys keep their
/// pre-#10386 behaviour. Red when an old OFF key stays open or an old ON key
/// is denied.
/// Test: itself.
#[test]
fn migration_keep_closed_denies_unflagged_and_opens_flagged() {
    let (_tmp, backend, index) = legacy_vault();
    migrate_vault(
        backend.as_ref(),
        &index,
        &vault(),
        AgentDenyMigration::KeepClosed,
    )
    .unwrap();
    assert!(item(&backend, ControlItem::AgentDeny, Some("OFF_KEY")));
    assert!(!item(&backend, ControlItem::AgentDeny, Some("ON_KEY")));
    assert!(!item(
        &backend,
        ControlItem::LegacyAgentsFlag,
        Some("ON_KEY")
    ));
    assert!(item(&backend, ControlItem::DenyMigrated, None));
    // Through the store: the same verdicts the agent gates read.
    let store = SecretStore::new(backend.clone(), index)
        .with_agent_deny_migration(AgentDenyMigration::KeepClosed);
    assert!(!store.agents_may_use(&vault(), &key("OFF_KEY")).unwrap());
    assert!(store.agents_may_use(&vault(), &key("ON_KEY")).unwrap());
}

/// Why: the migration is recorded per vault and runs once. A rerun must not
/// re-deny a key an operator opened since. Red when the marker is ignored.
/// Test: itself.
#[test]
fn migration_rerun_is_a_no_op() {
    let (_tmp, backend, index) = legacy_vault();
    migrate_vault(
        backend.as_ref(),
        &index,
        &vault(),
        AgentDenyMigration::KeepClosed,
    )
    .unwrap();
    // The operator opens OFF_KEY after the migration.
    backend
        .set_control_item(
            ControlItem::AgentDeny,
            &vault(),
            Some(&key("OFF_KEY")),
            false,
        )
        .unwrap();
    migrate_vault(
        backend.as_ref(),
        &index,
        &vault(),
        AgentDenyMigration::KeepClosed,
    )
    .unwrap();
    assert!(!item(&backend, ControlItem::AgentDeny, Some("OFF_KEY")));
    assert!(!item(&backend, ControlItem::AgentDeny, Some("ON_KEY")));
}

/// Why: Fail-Open Check — a marker that cannot be read must not let a
/// lookup through as "not denied". Red when the error is swallowed.
/// Test: itself.
#[test]
fn migration_unreadable_marker_refuses() {
    let (_tmp, backend, index) = legacy_vault();
    backend.fail_controls(true);
    let err = migrate_vault(
        backend.as_ref(),
        &index,
        &vault(),
        AgentDenyMigration::OpenAll,
    );
    assert!(matches!(err, Err(SecretsError::Backend { .. })), "{err:?}");
    let denied = agent_denied(
        backend.as_ref(),
        &index,
        &vault(),
        &key("ON_KEY"),
        AgentDenyMigration::OpenAll,
    );
    assert!(
        denied.is_err(),
        "an unreadable marker is never 'not denied'"
    );
}

/// Why: Fail-Open Check — a deny lookup that fails is an error, never "may
/// use". Red when `agents_may_use` maps an error to `true`.
/// Test: itself.
#[test]
fn agent_denied_fails_closed_when_the_lookup_fails() {
    let (_tmp, backend, index) = legacy_vault();
    let store = SecretStore::new(backend.clone(), index);
    assert!(store.agents_may_use(&vault(), &key("OFF_KEY")).unwrap());
    backend.fail_controls(true);
    assert!(store.agents_may_use(&vault(), &key("OFF_KEY")).is_err());
}

/// Why: a backend that cannot store a deny (1Password, Keeper) follows the
/// mode: open under open-all, denied under keep-closed, and a deny written
/// to it is refused rather than dropped. Red when either mode reads the
/// other's default, or a deny is accepted and lost.
/// Test: itself.
#[test]
fn agent_denied_without_controls_follows_the_mode() {
    let tmp = TempDir::new().unwrap();
    let index = NamesIndex::at(tmp.path().join("index"));
    index.upsert(&vault(), &key("K"), 10, 1).unwrap();
    let backend = MemoryBackend::without_agent_controls();
    for (mode, denied) in [
        (AgentDenyMigration::OpenAll, false),
        (AgentDenyMigration::KeepClosed, true),
    ] {
        assert_eq!(
            agent_denied(&backend, &index, &vault(), &key("K"), mode).unwrap(),
            denied,
            "{mode:?}"
        );
    }
    let store = SecretStore::new(Arc::new(backend), index);
    let deny = store.set_agents_may_use(&vault(), &key("K"), false);
    assert!(
        matches!(deny, Err(SecretsError::Unsupported { .. })),
        "{deny:?}"
    );
}
