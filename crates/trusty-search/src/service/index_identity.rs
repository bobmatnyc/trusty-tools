//! The index identity: `(instance_id, generation)`, persisted per index (#10738).
//!
//! Why: a caller that journals "this path is indexed" needs to know when that
//! record stopped being true without a per-path remove — the index was deleted
//! and re-created, or its corpus was reset. `search.index.status` named nothing
//! that changes on either event (SPEC-OKF-01 §4.9.2, dependency D1).
//! What: one `<data_dir>/index-identity/<id>.json` per index. It sits outside
//! `indexes/<id>/` so a status read never creates a store directory, which
//! #8499's work-tree guard reads as an existing store.
//! - `instance_id`: 128 random bits, lowercase hex. Minted the first time the
//!   identity is read; kept across restarts and ordinary writes. A delete
//!   retires it, so a re-created index mints a new one. A file that cannot be
//!   read or parsed is replaced by a new mint, never a guessed old value.
//! - `generation`: starts at 1 per `instance_id`. [`bump_generation`] raises it
//!   when the stored contents may have lost chunks without a per-path remove:
//!   a full reindex that resets the hash cache, a force promotion, a migration
//!   fault, a corpus open that falls back to no corpus, a corpus created empty
//!   in place of one that held chunks. The bump is written and fsynced before
//!   it returns.
//! - A failed bump or retire poisons the index in this process: every read
//!   answers an error, never the pre-event value, until a bump or retire
//!   succeeds. Across a restart the corpus `_meta` stamp and pending marker
//!   make the next attach bump again ([`reconcile_on_attach`]).
//!
//! Test: `service::rpc::identity_10738_tests`,
//! `service::persistence_loader::identity_10738_tests`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The directory under the data dir that holds one identity file per index.
const IDENTITY_DIR: &str = "index-identity";

/// Serializes every read-modify-write of an identity file in this process,
/// and holds the poisoned indexes: id → why its identity is unavailable.
// #10738: a failed bump or retire must not fall back to the stale file.
static IDENTITY_LOCK: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// One index's identity, as `search.index.status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexIdentity {
    /// Opaque, at least 128 random bits; never reused.
    pub instance_id: String,
    /// Raised on every event that can drop chunks without a per-path remove.
    pub generation: u64,
}

impl IndexIdentity {
    /// A fresh identity: new random `instance_id`, generation 1.
    fn mint() -> Self {
        let bytes: [u8; 16] = rand::random();
        let instance_id = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            instance_id,
            generation: 1,
        }
    }

    /// A stored value is usable only when it could have come from [`Self::mint`].
    fn is_well_formed(&self) -> bool {
        self.generation >= 1
            && self.instance_id.len() >= 32
            && self.instance_id.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

fn lock() -> std::sync::MutexGuard<'static, BTreeMap<String, String>> {
    IDENTITY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn identity_path(index_id: &str) -> Result<PathBuf> {
    let name = crate::service::persistence::sanitize_id_for_path(index_id);
    Ok(crate::service::persistence::data_dir()?
        .join(IDENTITY_DIR)
        .join(format!("{name}.json")))
}

/// Read the stored identity; `None` when absent, unreadable or malformed.
fn read(path: &Path) -> Option<IndexIdentity> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "index identity unreadable; minting a new one (#10738)");
            return None;
        }
    };
    match serde_json::from_slice::<IndexIdentity>(&raw) {
        Ok(id) if id.is_well_formed() => Some(id),
        _ => {
            tracing::warn!(path = %path.display(), "index identity malformed; minting a new one (#10738)");
            None
        }
    }
}

/// Write `identity` durably: temp file, fsync, rename.
fn write(path: &Path, identity: &IndexIdentity) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec(identity).context("serialize index identity")?;
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(&body)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("rename onto {}", path.display()))?;
    // #10738: fsync the directory so the rename itself survives a crash.
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .with_context(|| format!("fsync {}", dir.display()))?;
    }
    Ok(())
}

/// The index's identity, minting and persisting one when none is stored.
///
/// Why: every reply that carries `identity` reads it here, so a status read, a
/// put reply and a file-status reply cannot disagree.
/// What: returns the stored value; on absence or a malformed file, mints a new
/// one and writes it before returning. Fails when the new value cannot be
/// persisted, so a caller never sees an identity that a restart would change,
/// and while the index is poisoned by a failed bump or retire.
/// Test: `identity_is_stable_across_a_daemon_restart`,
/// `a_failed_bump_never_reports_the_pre_event_identity`.
pub fn current(index_id: &str) -> Result<IndexIdentity> {
    let poisoned = lock();
    // #10738: fail closed after a failed bump or retire.
    if let Some(why) = poisoned.get(index_id) {
        anyhow::bail!("index identity unavailable until the next successful bump: {why}");
    }
    let path = identity_path(index_id)?;
    if let Some(found) = read(&path) {
        return Ok(found);
    }
    let minted = IndexIdentity::mint();
    write(&path, &minted)?;
    Ok(minted)
}

/// Raise the index's generation and persist it before returning.
///
/// Why: SPEC-OKF-01 §5.6 D1 — any event that can leave a chunk absent without
/// a per-path remove must change the identity, durably, before the index
/// answers its next query.
/// What: adds 1 to the stored `generation`; with nothing stored, mints a new
/// identity, which no reply has carried. Writes it with fsync. On success the
/// index is no longer poisoned. On failure the index is poisoned, so every
/// later read is an error rather than the pre-event value, and the stale file
/// is removed when it can be. `reason` is logged.
/// Test: `generation_changes_when_a_force_reindex_resets_the_index`,
/// `a_failed_bump_never_reports_the_pre_event_identity`.
pub fn bump_generation(index_id: &str, reason: &str) -> Result<IndexIdentity> {
    let mut poisoned = lock();
    // #10738: every failure below poisons the index; none falls back to a read.
    let result = identity_path(index_id).and_then(|path| {
        let identity = match read(&path) {
            Some(mut stored) => {
                stored.generation = stored.generation.saturating_add(1);
                stored
            }
            None => IndexIdentity::mint(),
        };
        write(&path, &identity).map_err(|e| match std::fs::remove_file(&path) {
            Ok(()) => e,
            Err(r) if r.kind() == std::io::ErrorKind::NotFound => e,
            Err(r) => e.context(format!("stale identity not removed either: {r}")),
        })?;
        Ok(identity)
    });
    let identity = match result {
        Ok(identity) => identity,
        Err(e) => {
            poisoned.insert(
                index_id.to_string(),
                format!("bump for {reason} failed: {e:#}"),
            );
            return Err(e);
        }
    };
    poisoned.remove(index_id);
    tracing::info!(
        index_id,
        reason,
        generation = identity.generation,
        "index generation raised (#10738)"
    );
    Ok(identity)
}

/// [`bump_generation`] for call sites that cannot fail: logs an error instead.
pub fn bump_generation_logged(index_id: &str, reason: &str) {
    if let Err(e) = bump_generation(index_id, reason) {
        tracing::error!(index_id, reason, error = %format!("{e:#}"), "index generation bump failed (#10738)");
    }
}

/// Forget the index's identity, so a re-created index mints a new one.
///
/// Why: deleting and re-creating an index ID must change `instance_id`
/// (SPEC-OKF-01 §5.6 D1), even when the delete keeps the data directory.
/// What: removes the identity file; an absent file is success and lifts any
/// poison. A failed removal poisons the index, so the surviving file is never
/// reported again until a bump or retire succeeds.
/// Test: `identity_changes_when_the_index_is_deleted_and_recreated`,
/// `a_failed_retire_never_reports_the_retired_identity`.
pub fn retire(index_id: &str) -> Result<()> {
    let mut poisoned = lock();
    let result = identity_path(index_id).and_then(|path| match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    });
    // #10738: a retire that failed leaves the old file; never serve it again.
    match &result {
        Ok(()) => poisoned.remove(index_id),
        Err(e) => poisoned.insert(index_id.to_string(), format!("retire failed: {e:#}")),
    };
    result
}

/// Bump the identity when an attached corpus does not carry the stored one.
///
/// Why: the poison of a failed bump is in-memory, so a restart would read the
/// pre-event identity file back (§5.6 D1). The corpus `_meta` stamp says which
/// identity the corpus was attached under, and survives the restart.
/// What: bumps when the corpus was created empty, a bump is pending, or the
/// stamp is absent, unreadable or differs from the stored identity. Writes the
/// new stamp only after the bump succeeds; a failed bump leaves the index
/// poisoned and the stamp unchanged, so the next attach bumps again.
/// Test: `a_corpus_loss_bump_that_failed_is_retried_after_a_restart`,
/// `a_pending_bump_that_failed_is_retried_after_a_restart`,
/// `identity_changes_when_the_corpus_is_recreated_empty`.
pub(crate) fn reconcile_on_attach(index_id: &str, corpus: &crate::core::corpus::CorpusStore) {
    let Some(reason) = attach_bump_reason(index_id, corpus) else {
        return;
    };
    bump_and_stamp(index_id, Some(corpus), reason);
}

/// Why the attach of `corpus` must bump, or `None` when it carries the
/// stored identity.
fn attach_bump_reason(
    index_id: &str,
    corpus: &crate::core::corpus::CorpusStore,
) -> Option<&'static str> {
    if corpus.created_empty() {
        return Some("corpus_created_empty");
    }
    let (stamp, pending) = match corpus.read_identity_meta_sync() {
        Ok(meta) => meta,
        Err(e) => {
            tracing::warn!(index_id, error = %format!("{e:#}"), "identity stamp unreadable (#10738)");
            return Some("identity_stamp_unreadable");
        }
    };
    if pending {
        return Some("identity_bump_pending");
    }
    let Some(stamp) = stamp.and_then(|raw| serde_json::from_slice::<IndexIdentity>(&raw).ok())
    else {
        return Some("identity_stamp_absent");
    };
    match current(index_id) {
        Ok(stored) if stored == stamp => None,
        _ => Some("identity_stamp_mismatch"),
    }
}

/// Bump for an event that keeps the corpus: mark the bump pending in the
/// corpus first, so a restart retries a bump that did not complete.
///
/// Why: a root-move reset, a migration fault or a force reset keeps the corpus
/// file, so its stamp still matches the pre-event identity file (§5.6 D1).
/// What: writes `identity_bump_pending` when a corpus is wired, bumps, then
/// writes the new stamp and clears the marker. With no corpus it only bumps.
/// Failures are logged; a failed bump leaves the index poisoned.
/// Test: `a_pending_bump_that_failed_is_retried_after_a_restart`.
pub(crate) fn bump_with_pending(
    index_id: &str,
    corpus: Option<&crate::core::corpus::CorpusStore>,
    reason: &str,
) {
    if let Some(Err(e)) = corpus.map(|c| c.mark_identity_bump_pending_sync(reason)) {
        tracing::error!(index_id, reason, error = %format!("{e:#}"), "identity bump not marked pending (#10738)");
    }
    bump_and_stamp(index_id, corpus, reason);
}

/// Bump, then stamp `corpus` with the new identity on success.
fn bump_and_stamp(index_id: &str, corpus: Option<&crate::core::corpus::CorpusStore>, reason: &str) {
    let identity = match bump_generation(index_id, reason) {
        Ok(identity) => identity,
        Err(e) => {
            tracing::error!(index_id, reason, error = %format!("{e:#}"), "index generation bump failed (#10738)");
            return;
        }
    };
    let Some(corpus) = corpus else {
        return;
    };
    let stamped = serde_json::to_vec(&identity)
        .context("serialize index identity")
        .and_then(|raw| corpus.write_identity_stamp_sync(&raw));
    if let Err(e) = stamped {
        // The next attach sees a stale stamp or a pending marker and bumps.
        tracing::warn!(index_id, reason, error = %format!("{e:#}"), "identity stamp not written (#10738)");
    }
}

/// Drop this process's poison for `index_id`, as a daemon restart does.
#[cfg(test)]
pub(crate) fn forget_process_state(index_id: &str) {
    lock().remove(index_id);
}

/// The `identity` field of a reply: the identity, or `null` plus the reason.
///
/// Why: a reply must never carry a guessed identity (§5.6 D1). When the
/// identity cannot be read or minted, the field is `null` and
/// `identity_error` says why, so a caller treats the index as unreadable.
/// What: inserts `identity` (and, on failure, `identity_error`) into `body`.
/// Test: `identity_is_stable_across_a_daemon_restart`.
pub fn insert_into(index_id: &str, body: &mut serde_json::Map<String, serde_json::Value>) {
    match current(index_id) {
        Ok(identity) => {
            body.insert("identity".into(), serde_json::json!(identity));
        }
        Err(e) => {
            body.insert("identity".into(), serde_json::Value::Null);
            body.insert("identity_error".into(), format!("{e:#}").into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IndexIdentity;
    use std::os::unix::fs::PermissionsExt;

    /// Makes the identity directory read-only, so a write and a remove in it
    /// both fail; restores it on drop, before the tempdir is removed.
    struct ReadOnly(std::path::PathBuf);
    impl ReadOnly {
        fn new(dir: std::path::PathBuf) -> Self {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))
                .expect("make the identity dir read-only");
            Self(dir)
        }
    }
    impl Drop for ReadOnly {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// The `identity` and `identity_error` fields a reply would carry now.
    fn reply_fields(id: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut body = serde_json::Map::new();
        super::insert_into(id, &mut body);
        body
    }

    /// #10738 (H2): a bump whose write AND stale-file removal both fail must
    /// not leave the pre-event identity readable; replies carry `null` plus
    /// `identity_error` until a bump succeeds, and that bump's value is new.
    #[test]
    #[serial_test::serial]
    fn a_failed_bump_never_reports_the_pre_event_identity() {
        // Root ignores directory permissions, so nothing would fail.
        if nix::unistd::geteuid().is_root() {
            return;
        }
        let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
        let id = "identity-bump-fails-10738";
        let before = super::current(id).expect("mint the identity");
        let guard = ReadOnly::new(isolated.path().join(super::IDENTITY_DIR));

        super::bump_generation(id, "test").expect_err("a read-only dir must fail the bump");
        let err = super::current(id).expect_err("the pre-event identity must not be read back");
        assert!(
            format!("{err:#}").contains("bump for test failed"),
            "{err:#}"
        );
        let body = reply_fields(id);
        assert!(body["identity"].is_null(), "{body:?}");
        assert!(body["identity_error"].is_string(), "{body:?}");

        drop(guard);
        let after = super::bump_generation(id, "retry").expect("a writable dir bumps");
        assert_ne!(after, before);
        assert_eq!(super::current(id).expect("healthy again"), after);
    }

    /// #10738 (H2): a retire whose removal fails must not leave the retired
    /// identity readable; replies carry `null` plus `identity_error` until a
    /// retire succeeds, after which a new `instance_id` is minted.
    #[test]
    #[serial_test::serial]
    fn a_failed_retire_never_reports_the_retired_identity() {
        if nix::unistd::geteuid().is_root() {
            return;
        }
        let isolated = crate::service::server::tests_components::IsolatedDataDir::new();
        let id = "identity-retire-fails-10738";
        let before = super::current(id).expect("mint the identity");
        let guard = ReadOnly::new(isolated.path().join(super::IDENTITY_DIR));

        super::retire(id).expect_err("a read-only dir must fail the retire");
        super::current(id).expect_err("the retired identity must not be read back");
        assert!(reply_fields(id)["identity"].is_null());

        drop(guard);
        super::retire(id).expect("a writable dir retires");
        let after = super::current(id).expect("a new mint");
        assert_ne!(after.instance_id, before.instance_id);
    }

    /// A mint carries 128 bits of hex and starts at generation 1; two differ.
    #[test]
    fn a_mint_is_well_formed_and_unique() {
        let a = IndexIdentity::mint();
        let b = IndexIdentity::mint();
        assert!(a.is_well_formed(), "{a:?}");
        assert_eq!(a.instance_id.len(), 32);
        assert_eq!(a.generation, 1);
        assert_ne!(a.instance_id, b.instance_id);
    }
}
