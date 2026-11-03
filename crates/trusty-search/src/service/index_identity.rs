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
//!   fault, a corpus open that falls back to no corpus. The bump is written and
//!   fsynced before it returns.
//!
//! Test: `service::rpc::identity_10738_tests`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The directory under the data dir that holds one identity file per index.
const IDENTITY_DIR: &str = "index-identity";

/// Serializes every read-modify-write of an identity file in this process.
static IDENTITY_LOCK: Mutex<()> = Mutex::new(());

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

fn lock() -> std::sync::MutexGuard<'static, ()> {
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
    Ok(())
}

/// The index's identity, minting and persisting one when none is stored.
///
/// Why: every reply that carries `identity` reads it here, so a status read, a
/// put reply and a file-status reply cannot disagree.
/// What: returns the stored value; on absence or a malformed file, mints a new
/// one and writes it before returning. Fails only when the new value cannot be
/// persisted, so a caller never sees an identity that a restart would change.
/// Test: `identity_is_stable_across_a_daemon_restart`.
pub fn current(index_id: &str) -> Result<IndexIdentity> {
    let _guard = lock();
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
/// What: reads (or mints) the identity, adds 1 to `generation`, writes it with
/// fsync. If the write fails, removes the file so the next read mints a new
/// `instance_id` — a stale identity must never survive a reset. `reason` is
/// logged.
/// Test: `generation_changes_when_a_force_reindex_resets_the_index`.
pub fn bump_generation(index_id: &str, reason: &str) -> Result<IndexIdentity> {
    let _guard = lock();
    let path = identity_path(index_id)?;
    let mut identity = read(&path).unwrap_or_else(IndexIdentity::mint);
    identity.generation = identity.generation.saturating_add(1);
    if let Err(e) = write(&path, &identity) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
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
/// What: removes the identity file; an absent file is success.
/// Test: `identity_changes_when_the_index_is_deleted_and_recreated`.
pub fn retire(index_id: &str) -> Result<()> {
    let _guard = lock();
    let path = identity_path(index_id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
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
