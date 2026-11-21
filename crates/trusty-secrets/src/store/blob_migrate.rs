//! `secrets.migrate`: move per-item Keychain items into the vault blob, and
//! back (#10524, owner ruling 2026-11-19).
//!
//! Why: the one-prompt unlock needs every trusty Keychain item inside the
//! one blob item; owners already hold per-item items, and none may be lost
//! on the way, or on the way back.
//! What: three steps, each under the blob's write lock, so no backend write
//! in this process interleaves with one.
//! - [`migrate`]: read every per-item item the index names; write the backup
//!   item and verify it by read-back (same ids, same values, compared in
//!   memory); write the blob and verify it the same way. A failed blob step
//!   deletes the blob again, so reads stay on the per-item items. The old
//!   items are never deleted here. With a blob present it changes nothing.
//! - [`finish`]: with a blob and a backup present, delete the old per-item
//!   items. A failure stops it, names how many went, and leaves the rest;
//!   running it again continues.
//! - [`rollback`]: with a backup present, write every item of the current
//!   blob (or of the backup, when the blob is corrupt) back to its own item,
//!   verify each by read-back, and only then delete the blob. Without a
//!   backup it changes nothing.
//!
//! Every step has a dry run that writes nothing. Reports carry counts and
//! key names, never a value.
//! Test: `blob_migrate_tests.rs` beside this file.

use super::ControlItem;
use super::blob_keychain::{control_item_id, value_item};
use super::vault_blob::{Blob, BlobStore, ItemId, item_error};
use crate::api::{SecretKey, SecretsError, VaultName};

/// The control items a key can carry.
const KEY_CONTROLS: [ControlItem; 3] = [
    ControlItem::AgentDeny,
    ControlItem::LegacyAgentsFlag,
    ControlItem::PushOnly,
];

/// What one step did, or would do on a dry run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StepReport {
    /// Items moved (migrate, rollback) or old items deleted (finish).
    pub(crate) items: usize,
    /// The `vault/key` names of the value items among them, sorted.
    pub(crate) keys: Vec<String>,
    /// Nothing to do: already migrated (migrate) or already per-item
    /// (rollback).
    pub(crate) already: bool,
    /// Rollback's source: `blob`, or `backup` when the blob is corrupt.
    pub(crate) source: Option<&'static str>,
}

/// Every per-item item the index can name: per vault its deny-migration
/// marker; per key its value and its control items.
pub(crate) fn legacy_items(vaults: &[(VaultName, Vec<SecretKey>)]) -> Vec<ItemId> {
    let mut ids = Vec::new();
    for (vault, keys) in vaults {
        ids.push(control_item_id(ControlItem::DenyMigrated, vault, None));
        for key in keys {
            ids.push(value_item(vault, key));
            ids.extend(
                KEY_CONTROLS
                    .iter()
                    .map(|item| control_item_id(*item, vault, Some(key))),
            );
        }
    }
    ids
}

/// The report for `blob`: its size and its value items' names.
fn report_of(blob: &Blob) -> StepReport {
    let mut keys: Vec<String> = blob
        .items()
        .filter(|(id, _)| id.service.starts_with("trusty/"))
        .map(|(id, _)| format!("{}/{}", id.service, id.account))
        .collect();
    keys.sort();
    StepReport {
        items: blob.len(),
        keys,
        ..StepReport::default()
    }
}

/// Read item `id` back and require it to decode to exactly `expected`.
fn verify(store: &BlobStore, id: &ItemId, expected: &Blob) -> Result<(), SecretsError> {
    match store.read_blob_at(id)? {
        Some(read) if read.same_as(expected) => Ok(()),
        Some(_) => Err(item_error(
            id,
            "the read-back does not match what was written",
        )),
        None => Err(item_error(id, "the item is missing on read-back")),
    }
}

/// Move every present item in `legacy` into the blob, behind a verified
/// backup; on a dry run, only report what would move.
///
/// What: see the module docs. Any error leaves the old items in place and
/// no blob behind (a leftover blob that cannot be deleted is named in the
/// error, so `rollback` can clear it).
pub(crate) fn migrate(
    store: &BlobStore,
    legacy: &[ItemId],
    dry_run: bool,
) -> Result<StepReport, SecretsError> {
    let _writes = store.write_lock();
    if let Some(blob) = store.read_blob()? {
        return Ok(StepReport {
            already: true,
            ..report_of(&blob)
        });
    }
    let mut blob = Blob::default();
    for id in legacy {
        if let Some(value) = store.items().read(id)? {
            blob.insert(id.clone(), &value);
        }
    }
    let report = report_of(&blob);
    if dry_run {
        return Ok(report);
    }
    let text = blob.encode()?;
    store.items().write(&ItemId::backup(), &text)?;
    verify(store, &ItemId::backup(), &blob)?;
    let landed = store
        .items()
        .write(&ItemId::blob(), &text)
        .and_then(|()| verify(store, &ItemId::blob(), &blob));
    if let Err(err) = landed {
        // #10524: no blob, so every read stays on the per-item items.
        return Err(match store.items().delete(&ItemId::blob()) {
            Ok(_) => err,
            Err(_) => item_error(
                &ItemId::blob(),
                "the blob failed verification and could not be removed; run \
                 `tm secrets migrate --rollback`",
            ),
        });
    }
    Ok(report)
}

/// Delete the old per-item items once the blob and its backup exist.
///
/// What: refuses, deleting nothing, without a blob or a backup. Counts only
/// items that existed. Stops at the first failure, naming the count so far.
pub(crate) fn finish(
    store: &BlobStore,
    legacy: &[ItemId],
    dry_run: bool,
) -> Result<StepReport, SecretsError> {
    let _writes = store.write_lock();
    let Some(blob) = store.read_blob()? else {
        return Err(item_error(&ItemId::blob(), "not migrated: no vault blob"));
    };
    if store.read_blob_at(&ItemId::backup())?.is_none() {
        return Err(item_error(&ItemId::backup(), "no migration backup"));
    }
    let mut report = report_of(&blob);
    if dry_run {
        report.items = legacy.len();
        return Ok(report);
    }
    let mut deleted = 0;
    for id in legacy {
        match store.items().delete(id) {
            Ok(existed) => deleted += usize::from(existed),
            Err(SecretsError::Backend { reason, .. }) => {
                return Err(item_error(
                    id,
                    format!("finish stopped after deleting {deleted} old items: {reason}"),
                ));
            }
            Err(err) => return Err(err),
        }
    }
    report.items = deleted;
    Ok(report)
}

/// Restore the per-item layout from the blob (or the backup).
///
/// What: see the module docs. A corrupt backup, a failed write or a
/// read-back mismatch stops it with the blob still in place, so reads are
/// unchanged.
pub(crate) fn rollback(store: &BlobStore, dry_run: bool) -> Result<StepReport, SecretsError> {
    let _writes = store.write_lock();
    let Some(backup) = store.read_blob_at(&ItemId::backup())? else {
        return Err(item_error(
            &ItemId::backup(),
            "no migration backup; nothing was changed",
        ));
    };
    let Some(raw) = store.items().read(&ItemId::blob())? else {
        return Ok(StepReport {
            already: true,
            ..StepReport::default()
        });
    };
    let (source, name) = match Blob::decode(&ItemId::blob(), &raw) {
        Ok(blob) => (blob, "blob"),
        // #10524: the blob is unreadable anyway; the backup is the recovery.
        Err(_) => (backup, "backup"),
    };
    let report = StepReport {
        source: Some(name),
        ..report_of(&source)
    };
    if dry_run {
        return Ok(report);
    }
    for (id, value) in source.items() {
        store.items().write(id, value)?;
        let back = store.items().read(id)?;
        if back.as_deref().map(String::as_str) != Some(value) {
            return Err(item_error(
                id,
                "the read-back does not match what was written",
            ));
        }
    }
    store.items().delete(&ItemId::blob())?;
    Ok(report)
}

#[cfg(test)]
#[path = "blob_migrate_tests.rs"]
mod tests;
