//! Tests for the vault blob migration (#10524). Every store is the
//! in-memory fake; none touches the login Keychain. Each failure-arm test
//! asserts the old items still read, so a variant that went on past the
//! failure (a fail-open migration) goes red.
//!
//! Test: itself.

use std::sync::Arc;

use super::*;
use crate::store::mem_items::MemItems;
use crate::store::vault_blob::ItemStore;

const VALUE_A: &str = "sk-fake-migrate-a1";
const VALUE_B: &str = "sk-fake-migrate-b2";

fn vault() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

/// The legacy ids of `A_KEY` and `B_KEY` in the vault.
fn legacy() -> Vec<ItemId> {
    legacy_items(&[(vault(), vec![key("A_KEY"), key("B_KEY")])])
}

/// Per-item items: two values and `A_KEY`'s deny.
fn seeded() -> (Arc<MemItems>, BlobStore) {
    let deny = control_item_id(ControlItem::AgentDeny, &vault(), Some(&key("A_KEY")));
    let items = Arc::new(MemItems::with(&[
        (&value_item(&vault(), &key("A_KEY")), VALUE_A),
        (&value_item(&vault(), &key("B_KEY")), VALUE_B),
        (&deny, "1"),
    ]));
    let store = BlobStore::new(Arc::clone(&items) as Arc<dyn ItemStore>);
    (items, store)
}

fn a() -> ItemId {
    value_item(&vault(), &key("A_KEY"))
}

/// The old per-item layout still answers: no blob, `A_KEY` reads its value.
fn still_per_item(items: &MemItems, store: &BlobStore) {
    assert!(
        items.raw(&ItemId::blob()).is_none(),
        "a blob was left behind"
    );
    assert_eq!(store.get(&a()).unwrap().unwrap().as_str(), VALUE_A);
}

/// Why: #10524 AC2 — migrate writes a verified backup and blob holding every
/// present item, keeps the old items, and a second run changes nothing; a
/// dry run writes nothing at all.
/// Test: itself.
#[test]
fn migrate_moves_every_item_behind_a_backup_and_is_idempotent() {
    let (items, store) = seeded();
    let dry = migrate(&store, &legacy(), true).unwrap();
    assert_eq!(dry.items, 3);
    assert!(items.raw(&ItemId::backup()).is_none(), "a dry run wrote");
    still_per_item(&items, &store);

    let report = migrate(&store, &legacy(), false).unwrap();
    assert_eq!(report.items, 3);
    assert_eq!(
        report.keys,
        ["trusty/acme/web/A_KEY", "trusty/acme/web/B_KEY"]
    );
    let backup = store.read_blob_at(&ItemId::backup()).unwrap().unwrap();
    assert!(backup.same_as(&store.read_blob().unwrap().unwrap()));
    assert_eq!(
        items.raw(&a()).as_deref(),
        Some(VALUE_A),
        "an old item was deleted"
    );
    assert_eq!(store.get(&a()).unwrap().unwrap().as_str(), VALUE_A);

    let again = migrate(&store, &legacy(), false).unwrap();
    assert!(again.already);
    assert_eq!(again.items, 3);
}

/// Why: #10524 AC2 error arms — a backup or blob step that fails, or reads
/// back other than it wrote, leaves no blob, so every read stays on the old
/// items. A variant that wrote the blob anyway, or kept a mismatched one,
/// fails `still_per_item`.
/// Test: itself.
#[test]
fn migrate_failure_at_any_step_leaves_the_old_items_readable() {
    type Fault = fn(&MemItems);
    let arms: [(&str, Fault); 4] = [
        ("backup write fails", |m| {
            m.fail_write.lock().unwrap().insert(ItemId::backup());
        }),
        ("backup verify mismatch", |m| {
            m.garble_write.lock().unwrap().insert(ItemId::backup());
        }),
        ("blob write fails", |m| {
            m.fail_write.lock().unwrap().insert(ItemId::blob());
        }),
        ("blob verify mismatch", |m| {
            m.garble_write.lock().unwrap().insert(ItemId::blob());
        }),
    ];
    for (arm, arm_fault) in arms {
        let (items, store) = seeded();
        arm_fault(&items);
        let err = migrate(&store, &legacy(), false).unwrap_err().to_string();
        assert!(
            !err.contains(VALUE_A) && !err.contains(VALUE_B),
            "{arm}: {err}"
        );
        still_per_item(&items, &store);
        assert_eq!(
            items
                .raw(&ItemId::new("trusty/acme/web", "B_KEY"))
                .as_deref(),
            Some(VALUE_B),
            "{arm}"
        );
    }
}

/// Why: #10524 — a mismatched blob that cannot be deleted must say so and
/// name the recovery; reads then fail closed rather than read a short blob.
/// Test: itself.
#[test]
fn migrate_blob_mismatch_that_cannot_be_removed_names_rollback() {
    let (items, store) = seeded();
    items.garble_write.lock().unwrap().insert(ItemId::blob());
    *items.delete_budget.lock().unwrap() = Some(0);
    let err = migrate(&store, &legacy(), false).unwrap_err().to_string();
    assert!(err.contains("--rollback"), "{err}");
    assert_eq!(items.raw(&a()).as_deref(), Some(VALUE_A));
}

/// Why: #10524 AC2 — finish deletes the old items only with a blob and a
/// backup present; a delete that fails midway stops, names the count, and
/// leaves the rest; running it again completes. A variant that skipped the
/// failure and reported success fails the error assertion.
/// Test: itself.
#[test]
fn finish_stops_midway_on_a_failed_delete_and_resumes() {
    let (items, store) = seeded();
    assert!(
        finish(&store, &legacy(), false).is_err(),
        "finished with no blob"
    );
    still_per_item(&items, &store);
    migrate(&store, &legacy(), false).unwrap();
    *items.delete_budget.lock().unwrap() = Some(1);
    let err = finish(&store, &legacy(), false).unwrap_err().to_string();
    assert!(err.contains("after deleting"), "{err}");
    assert_eq!(
        items
            .raw(&ItemId::new("trusty/acme/web", "B_KEY"))
            .as_deref(),
        Some(VALUE_B)
    );
    assert_eq!(store.get(&a()).unwrap().unwrap().as_str(), VALUE_A);
    *items.delete_budget.lock().unwrap() = None;
    finish(&store, &legacy(), false).unwrap();
    assert!(items.raw(&a()).is_none());
    assert_eq!(store.get(&a()).unwrap().unwrap().as_str(), VALUE_A);
}

/// Why: #10524 AC2 — finish refuses with no backup, deleting nothing.
/// Test: itself.
#[test]
fn finish_without_a_backup_deletes_nothing() {
    let (items, store) = seeded();
    migrate(&store, &legacy(), false).unwrap();
    items.delete(&ItemId::backup()).unwrap();
    assert!(finish(&store, &legacy(), false).is_err());
    assert_eq!(items.raw(&a()).as_deref(), Some(VALUE_A));
}

/// Why: #10524 AC2 rollback — restores every blob item (with writes made
/// after the migration) to its own item, then deletes the blob; the backup
/// stays. A dry run changes nothing; a second run is a no-op.
/// Test: itself.
#[test]
fn rollback_restores_the_per_item_layout_with_later_writes() {
    let (items, store) = seeded();
    migrate(&store, &legacy(), false).unwrap();
    finish(&store, &legacy(), false).unwrap();
    store.put(&a(), Some("after-migrate")).unwrap();
    let dry = rollback(&store, true).unwrap();
    assert_eq!((dry.items, dry.source), (3, Some("blob")));
    assert!(items.raw(&a()).is_none(), "a dry run wrote");
    rollback(&store, false).unwrap();
    assert!(items.raw(&ItemId::blob()).is_none());
    assert!(items.raw(&ItemId::backup()).is_some());
    assert_eq!(store.get(&a()).unwrap().unwrap().as_str(), "after-migrate");
    assert!(rollback(&store, false).unwrap().already);
}

/// Why: #10524 rollback error arms — no backup changes nothing; a failed
/// item write stops with the blob still in place, so reads are unchanged; a
/// corrupt blob restores from the backup. A variant that deleted the blob
/// after a failed write would lose `B_KEY` and fail this test.
/// Test: itself.
#[test]
fn rollback_fails_closed_without_a_backup_or_on_a_failed_write() {
    let (items, store) = seeded();
    migrate(&store, &legacy(), false).unwrap();
    finish(&store, &legacy(), false).unwrap();
    let backup = items.raw(&ItemId::backup()).unwrap();
    items.delete(&ItemId::backup()).unwrap();
    assert!(
        rollback(&store, false).is_err(),
        "rolled back with no backup"
    );
    assert!(items.raw(&ItemId::blob()).is_some());

    items.put_raw(&ItemId::backup(), &backup);
    let b = ItemId::new("trusty/acme/web", "B_KEY");
    items.fail_write.lock().unwrap().insert(b.clone());
    assert!(rollback(&store, false).is_err());
    assert!(
        items.raw(&ItemId::blob()).is_some(),
        "the blob went after a failed write"
    );
    assert_eq!(store.get(&b).unwrap().unwrap().as_str(), VALUE_B);

    items.fail_write.lock().unwrap().clear();
    items.put_raw(&ItemId::blob(), "corrupt");
    let report = rollback(&store, false).unwrap();
    assert_eq!(report.source, Some("backup"));
    assert_eq!(store.get(&b).unwrap().unwrap().as_str(), VALUE_B);
}
