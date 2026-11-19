//! The vault blob through the server: `secrets.migrate`, then one Keychain
//! read at `secrets.unlock` (#10524, owner ruling 2026-11-19).
//!
//! A child of `session_cache_tests`, so it shares the fixture, the fake
//! process table and the method-body helpers. The Keychain is the blob
//! backend over the in-memory item store; no test reads or writes the login
//! Keychain. The fake's read counter stands in for prompts: every Keychain
//! read is one.
//!
//! Test: itself.

use crate::server::audit::MigrationAudit;
use crate::server::migrate;
use crate::server::unlock;
use crate::store::BlobKeychain;
use crate::store::grant_cache::{DropReason, Status};
use crate::store::mem_items::MemItems;
use crate::store::vault_blob::{BlobStore, ItemId, ItemStore};

use super::*;

const KEYS: [&str; 3] = ["KEY_ONE", "KEY_TWO", "KEY_THREE"];

/// A state whose Keychain is the blob backend over `items`, holding
/// [`KEYS`] in the per-item layout, and an exec grant for them.
fn blob_state(fx: &Fixture) -> (Arc<MemItems>, State, String) {
    let items = Arc::new(MemItems::default());
    let store = Arc::new(BlobStore::new(Arc::clone(&items) as Arc<dyn ItemStore>));
    let kc: Arc<dyn SecretBackend> = Arc::new(BlobKeychain::new(store));
    let seed = SecretStore::new(Arc::clone(&kc), NamesIndex::at(&fx.settings.index_root));
    for (n, name) in KEYS.iter().enumerate() {
        let value = SecretValue::new(format!("{SENTINEL_VALUE}-{n}"));
        seed.set(&vault(PROJECT_VAULT), &key(name), &value).unwrap();
    }
    select(fx, "keychain");
    let base = fx.backends();
    let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
        "keychain" => Ok(Arc::clone(&kc)),
        _ => base(id),
    });
    let mut state = fx.state(factory);
    state.grants = Arc::new(GrantRegistry::new(
        procs_with_me(),
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    state.power = Arc::new(NoPower);
    let token = grant_in(&state, &fx.project(), &KEYS);
    (items, state, token)
}

/// A power source that starts, stays alive and never fires.
struct NoPower;

impl crate::server::power::PowerSource for NoPower {
    fn subscribe(
        &self,
        _: crate::server::power::PowerHook,
    ) -> Result<crate::server::power::Subscription, crate::server::power::WatchError> {
        Ok(crate::server::power::Subscription::new(|| ()))
    }

    fn alive(&self) -> bool {
        true
    }
}

fn step(state: &State, step: &str, dry_run: bool) -> Result<Value, ErrorKind> {
    migrate::migrate(
        state,
        caller(),
        json!({"project": "ignored", "step": step, "dry_run": dry_run}),
    )
}

/// Read every key in [`KEYS`] through `secrets.resolve`.
fn read_all(state: &State, token: &str) {
    for (n, name) in KEYS.iter().enumerate() {
        assert_eq!(
            value_of(state, token, name),
            format!("{SENTINEL_VALUE}-{n}")
        );
    }
}

fn migrations(fx: &Fixture) -> Vec<MigrationAudit> {
    records(fx)
        .into_iter()
        .filter(|r| r.method == AuditMethod::Migrate)
        .filter_map(|r| r.migration)
        .collect()
}

/// Why: #10524 AC1 — after `migrate`, `unlock` reads the Keychain exactly
/// once (the one prompt) and every later read of every key, values and the
/// agent-deny lookups alike, reads it no more; `lock` returns reads to the
/// Keychain. A variant that held no blob reads once per key and fails.
/// Test: itself.
#[test]
fn migrate_then_unlock_reads_the_keychain_once() {
    let fx = fixture();
    let (items, state, token) = blob_state(&fx);
    let moved = step(&state, "migrate", false).unwrap();
    assert_eq!(moved["keys"].as_array().unwrap().len(), KEYS.len());
    assert!(items.raw(&ItemId::blob()).is_some());

    let before = items.read_count();
    let answer = unlock::unlock(&state, caller(), json!({"project": "ignored"})).unwrap();
    assert_eq!(answer["blob_held"], true);
    assert_eq!(
        items.read_count(),
        before + 1,
        "unlock read more than the blob"
    );
    for _ in 0..2 {
        read_all(&state, &token);
    }
    assert_eq!(
        items.read_count(),
        before + 1,
        "a read under the grant prompted"
    );

    unlock::lock(&state, caller(), json!({})).unwrap();
    read_all(&state, &token);
    assert!(
        items.read_count() > before + 1,
        "a read after lock was served"
    );
}

/// Why: #10524 AC1 fail closed — a blob unlock cannot read, or cannot hold,
/// opens no grant, and the drop is audited as a fault. A variant that kept
/// the grant open without the blob fails the `Off` assertion.
/// Test: itself.
#[test]
fn unlock_with_an_unreadable_blob_opens_no_grant() {
    let fx = fixture();
    let (items, state, _) = blob_state(&fx);
    step(&state, "migrate", false).unwrap();
    items.fail_read.lock().unwrap().insert(ItemId::blob());
    let refused = unlock::unlock(&state, caller(), json!({"project": "ignored"}));
    assert_eq!(refused, Err(ErrorKind::BackendFailed));
    assert_eq!(state.cache.grant().check(), Status::Off);
    assert!(
        records(&fx)
            .iter()
            .any(|r| r.method == AuditMethod::GrantDrop && r.grant_drop == Some(DropReason::Fault))
    );
}

/// Why: #10524 — an agent, or an unreadable ancestry, never migrates; the
/// Keychain is untouched and the refusal is audited.
/// Test: itself.
#[test]
fn migrate_by_agent_descendant_is_refused_and_changes_nothing() {
    let fx = fixture();
    let (items, mut state, _) = blob_state(&fx);
    let procs = procs_with_me();
    procs.set_agent(10);
    state.grants = Arc::new(GrantRegistry::new(
        procs,
        Arc::new(FakeClock::at(T0)),
        DEFAULT_MAX_TTL,
    ));
    assert_eq!(
        step(&state, "migrate", false),
        Err(ErrorKind::AgentUseRefused)
    );
    let refused = migrate::migrate(
        &state,
        Caller::new(Some(999_999)),
        json!({"project": "x", "step": "migrate"}),
    );
    assert_eq!(refused, Err(ErrorKind::AgentUseRefused));
    assert!(items.raw(&ItemId::blob()).is_none());
    assert!(items.raw(&ItemId::backup()).is_none());
    let denied = records(&fx)
        .iter()
        .filter(|r| r.method == AuditMethod::Migrate && r.decision == AuditDecision::Deny)
        .count();
    assert_eq!(denied, 2);
}

/// Why: #10524 AC3 — each migrate, finish and rollback call leaves one
/// record with its step, counts and key names; a failed step adds a fixed
/// reason; no value reaches the audit log. A migration ends a live grant.
/// Test: itself.
#[test]
fn migrate_records_counts_and_names_never_values() {
    let fx = fixture();
    let (items, state, _) = blob_state(&fx);
    assert_eq!(
        step(&state, "rollback", false),
        Err(ErrorKind::BackendFailed)
    );
    step(&state, "migrate", true).unwrap();
    unlock::unlock(&state, caller(), json!({"project": "ignored"})).unwrap();
    step(&state, "migrate", false).unwrap();
    assert_eq!(
        state.cache.grant().check(),
        Status::Off,
        "a grant outlived the migration"
    );
    step(&state, "finish", false).unwrap();
    let back = step(&state, "rollback", false).unwrap();
    assert_eq!(back["source"], "blob");
    assert!(items.raw(&ItemId::blob()).is_none());

    let all = migrations(&fx);
    assert_eq!(all.len(), 5);
    assert!(
        all[0]
            .failure
            .as_deref()
            .is_some_and(|f| f.contains("no migration backup"))
    );
    assert!(all[1].dry_run);
    for audit in &all[1..] {
        assert_eq!(audit.keys.len(), KEYS.len(), "{audit:?}");
        assert!(audit.failure.is_none());
    }
    let text = std::fs::read_to_string(&fx.settings.audit_log).unwrap();
    assert!(
        !text.contains(SENTINEL_VALUE),
        "a value reached the audit log"
    );
}
