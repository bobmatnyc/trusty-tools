//! The on-disk grant store (`<state>/grants.json`), so a broker restart keeps grants (#11528).
//!
//! Why: grants lived in memory only, so every upgrade restart dropped them all
//! and every `browser_profiles` call returned `[]` until the user reconnected
//! in each profile.
//! What: [`GrantStore`] holds grant id, scope, profile id and browser incarnation
//! per active grant, never a key or ticket. [`GrantStore::save`] writes a temp
//! file (0600), fsyncs, and renames it over the store. [`GrantStore::load`]
//! fails closed: a symlink, a file not owner-only, bad JSON or one malformed
//! entry rejects the whole store. [`Core::restore_grants`] runs at broker start
//! and gives each restored grant the normal reconnect grace, so the existing
//! incarnation rule in `peer_back` decides whether it survives.
//! [`Core::persist_grants`] rewrites the store at every grant change; when the
//! rewrite fails the store is deleted, so a revoked grant can never return.
//!
//! No grant outlives its revocation (#11528 criterion c). Three cases cover it.
//! A rewrite that succeeds leaves a store without the revoked grant. A rewrite
//! that fails but whose delete succeeds leaves no store, so a restart restores
//! nothing. A rewrite AND delete that both fail leave a stale store on disk;
//! `Core::store_stale` then makes `Core::connect` refuse every new grant
//! (`GrantStoreStale`) until a later persist succeeds. That persist is retried
//! at each `retire`, at start, and at the head of each new-grant `connect`.
//! The flag lives under the broker mutex with the grants, so it cannot race
//! them. Remaining gap: the flag is in memory. If the broker restarts while a
//! stale store is still on disk, the store is well-formed, so `restore_grants`
//! loads it and the revoked grant returns inside its reconnect grace (the
//! incarnation rule still decides its fate). Closing that needs a durable
//! marker on a disk that just refused a write and a delete; not attempted.
//! Test: `store_round_trips_and_is_owner_only`, `a_rejected_store_restores_nothing`,
//! `a_failed_rewrite_deletes_the_store`, `a_failed_rewrite_and_delete_reports_a_stale_store`
//! here; `a_restart_keeps_a_grant_until_its_grace_ends`,
//! `a_revoked_grant_does_not_return_after_a_restart`,
//! `a_stale_store_refuses_new_grants_until_a_persist_succeeds` in `tests/broker_grants.rs`.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};

use crate::broker::{Core, Inner};
use crate::grants::{Grant, RECONNECT_GRACE};
use crate::pairing;

/// File name inside the state directory.
pub const FILE: &str = "grants.json";

/// Why the store could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An I/O failure.
    #[error("grant store I/O: {0}")]
    Io(#[from] io::Error),
    /// A symlink, a foreign owner, or a mode other than owner-only.
    #[error("grant store is not a private regular file: {0}")]
    Insecure(String),
    /// Bad JSON, or an entry that fails validation.
    #[error("grant store is malformed: {0}")]
    Malformed(String),
}

/// Why a store rewrite did not leave a trustworthy store.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The rewrite failed and the old store was deleted (or never existed).
    #[error("grant store rewrite failed, store deleted: {0}")]
    Deleted(io::Error),
    /// The rewrite failed AND the old store could not be deleted: it is stale.
    #[error(
        "grant store rewrite failed ({write}) and the stale store could not be deleted ({delete})"
    )]
    Stale {
        /// The rewrite error.
        write: io::Error,
        /// The delete error.
        delete: io::Error,
    },
}

/// Atomic write of the store text to a path.
pub type WriteFn = Arc<dyn Fn(&Path, &str) -> io::Result<()> + Send + Sync>;
/// Delete of the store at a path.
pub type RemoveFn = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

/// The two file operations of a rewrite, injectable so a test can fail them.
#[derive(Clone)]
pub struct StoreIo {
    /// Atomically write `text` to the path.
    pub write: WriteFn,
    /// Delete the file at the path.
    pub remove: RemoveFn,
}

impl Default for StoreIo {
    fn default() -> StoreIo {
        StoreIo {
            write: Arc::new(atomic_write),
            remove: Arc::new(|p| fs::remove_file(p)),
        }
    }
}

/// One persisted grant: no key, ticket or route secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredGrant {
    /// `g-` + 12 hex.
    pub grant: String,
    /// Always `profile`.
    pub scope: String,
    /// Profile id.
    pub profile: String,
    /// Browser incarnation the grant may rebind to.
    pub incarnation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    grants: Vec<StoredGrant>,
}

impl StoredGrant {
    fn validate(&self) -> Result<(), StoreError> {
        let id_ok = self.grant.strip_prefix("g-").is_some_and(|h| {
            h.len() == 12 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if !id_ok
            || self.scope != "profile"
            || self.profile.is_empty()
            || self.incarnation.is_empty()
        {
            return Err(StoreError::Malformed(format!("entry {:?}", self.grant)));
        }
        Ok(())
    }
}

/// The store file of one state directory.
#[derive(Debug, Clone)]
pub struct GrantStore {
    path: PathBuf,
}

/// Write `text` to `path` atomically: temp file, fsync, rename, fsync the directory.
fn atomic_write(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600))?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    fs::File::open(dir)?.sync_all()
}

impl GrantStore {
    /// The store under `state`.
    pub fn new(state: &Path) -> GrantStore {
        GrantStore {
            path: state.join(FILE),
        }
    }

    /// Read every stored grant; an absent file is an empty store.
    pub fn load(&self) -> Result<Vec<StoredGrant>, StoreError> {
        let meta = match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
            Ok(m) => m,
        };
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != me {
            return Err(StoreError::Insecure(self.path.display().to_string()));
        }
        let doc: Document = serde_json::from_slice(&fs::read(&self.path)?)
            .map_err(|e| StoreError::Malformed(e.to_string()))?;
        let mut seen = std::collections::HashSet::new();
        for g in &doc.grants {
            g.validate()?;
            if !seen.insert(g.grant.as_str()) {
                return Err(StoreError::Malformed(format!("duplicate {:?}", g.grant)));
            }
        }
        if doc.version != 1 {
            return Err(StoreError::Malformed(format!("version {}", doc.version)));
        }
        Ok(doc.grants)
    }

    /// Replace the store with `grants` atomically.
    pub fn save(&self, grants: &[StoredGrant]) -> Result<(), SaveError> {
        self.replace(grants, &StoreIo::default())
    }

    /// [`GrantStore::save`] with the file operations injectable; a failed write deletes the store.
    ///
    /// Why: a stale store could bring back a revoked grant.
    /// What: on a write error the file is removed. A removal that fails with
    /// anything but `NotFound` returns [`SaveError::Stale`], so the caller can fail closed.
    /// Test: `a_failed_rewrite_deletes_the_store`, `a_failed_rewrite_and_delete_reports_a_stale_store`.
    pub fn replace(&self, grants: &[StoredGrant], io: &StoreIo) -> Result<(), SaveError> {
        let doc = Document {
            version: 1,
            grants: grants.to_vec(),
        };
        let text =
            serde_json::to_string(&doc).map_err(|e| SaveError::Deleted(io::Error::other(e)))?;
        let Err(write) = (io.write)(&self.path, &text) else {
            return Ok(());
        };
        // #11528: never leave an older store that still names a now-ended grant.
        match (io.remove)(&self.path) {
            Ok(()) => Err(SaveError::Deleted(write)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(SaveError::Deleted(write)),
            Err(delete) => Err(SaveError::Stale { write, delete }),
        }
    }
}

impl Core {
    /// Load the store at broker start and re-create each grant in its reconnect grace.
    ///
    /// Why: a restart must not drop grants (#11528).
    /// What: a rejected store restores nothing. A grant whose profile is no
    /// longer paired is dropped. Each restored grant is bound to no worker and
    /// ends at `now + RECONNECT_GRACE` unless a worker with the stored
    /// incarnation attaches first.
    /// Test: `a_restart_keeps_a_grant_until_its_grace_ends` in `tests/broker_grants.rs`.
    pub fn restore_grants(&self, inner: &mut Inner) {
        let store = GrantStore::new(&self.state);
        let stored = match store.load() {
            Ok(stored) => stored,
            Err(e) => {
                eprintln!("trusty-browser: grants not restored: {e}");
                return;
            }
        };
        let until = self.clock.now() + RECONNECT_GRACE;
        for s in stored {
            let route = match pairing::profile_row(&inner.db, &s.profile) {
                Ok(Some(route)) => route,
                _ => continue,
            };
            inner.grants.push(Grant {
                ident: s.grant,
                scope: s.scope,
                route,
                tab: None,
                // An empty generation matches no worker, so `peer_back` rebinds it.
                generation: Some(String::new()),
                active: true,
                sessions: Vec::new(),
                last_used: self.clock.now(),
                reason: None,
                incarnation: Some(s.incarnation),
                grace_until: Some(until),
            });
        }
        self.persist_grants(inner);
        self.arm(inner);
    }

    /// Rewrite the store from the active grants; on failure delete it.
    ///
    /// Why: a stale store would bring a revoked grant back after a restart.
    /// What: sets `Core::store_stale` when the rewrite and the delete both fail,
    /// clears it on any other outcome (the store is then current or gone).
    /// Test: `a_stale_store_refuses_new_grants_until_a_persist_succeeds`.
    pub fn persist_grants(&self, inner: &mut Inner) {
        let grants: Vec<StoredGrant> = inner
            .grants
            .iter()
            .filter(|g| g.active)
            .filter_map(|g| {
                Some(StoredGrant {
                    grant: g.ident.clone(),
                    scope: g.scope.clone(),
                    profile: g.route.profile.clone(),
                    incarnation: g.incarnation.clone().filter(|i| !i.is_empty())?,
                })
            })
            .collect();
        // #11528: a stale store (rewrite and delete both failed) makes `connect` fail closed.
        let saved = GrantStore::new(&self.state).replace(&grants, &self.store_io);
        if let Err(e) = &saved {
            eprintln!("trusty-browser: {e}");
        }
        self.store_stale.store(
            matches!(saved, Err(SaveError::Stale { .. })),
            Ordering::SeqCst,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one() -> StoredGrant {
        StoredGrant {
            grant: "g-0123456789ab".into(),
            scope: "profile".into(),
            profile: "p-1".into(),
            incarnation: "inc-1".into(),
        }
    }

    fn store() -> (tempfile::TempDir, GrantStore) {
        let dir = tempfile::tempdir().expect("dir");
        let s = GrantStore::new(dir.path());
        (dir, s)
    }

    #[test]
    fn store_round_trips_and_is_owner_only() {
        let (_d, s) = store();
        assert!(s.load().expect("absent").is_empty());
        s.save(&[one()]).expect("save");
        assert_eq!(s.load().expect("load"), vec![one()]);
        let mode = fs::metadata(&s.path).expect("meta").mode() & 0o777;
        assert_eq!(mode, 0o600);
        let text = fs::read_to_string(&s.path).expect("text");
        assert!(!text.contains("key") && !text.contains("ticket"), "{text}");
    }

    #[test]
    fn a_rejected_store_restores_nothing() {
        let good = r#"{"version":1,"grants":[{"grant":"g-0123456789ab","scope":"profile","profile":"p-1","incarnation":"inc-1"}]}"#;
        let bad_entry = good.replace("g-0123456789ab", "g-nothex");
        type Setup<'a> = Box<dyn Fn(&GrantStore) + 'a>;
        let cases: Vec<(&str, Setup<'_>)> = vec![
            (
                "mode 0644",
                Box::new(|s| {
                    fs::write(&s.path, good).expect("w");
                    fs::set_permissions(&s.path, fs::Permissions::from_mode(0o644)).expect("m");
                }),
            ),
            (
                "symlink",
                Box::new(|s| {
                    let real = s.path.with_extension("real");
                    fs::write(&real, good).expect("w");
                    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).expect("m");
                    std::os::unix::fs::symlink(&real, &s.path).expect("link");
                }),
            ),
            (
                "bad json",
                Box::new(|s| {
                    fs::write(&s.path, "{not json").expect("w");
                    fs::set_permissions(&s.path, fs::Permissions::from_mode(0o600)).expect("m");
                }),
            ),
            (
                "malformed entry",
                Box::new(|s| {
                    fs::write(&s.path, &bad_entry).expect("w");
                    fs::set_permissions(&s.path, fs::Permissions::from_mode(0o600)).expect("m");
                }),
            ),
        ];
        for (name, setup) in cases {
            let (_d, s) = store();
            setup(&s);
            assert!(s.load().is_err(), "{name} must reject the whole store");
        }
    }

    #[test]
    fn a_failed_rewrite_deletes_the_store() {
        let (_d, s) = store();
        s.save(&[one()]).expect("save");
        let io = StoreIo {
            write: Arc::new(|_, _| Err(io::Error::other("disk full"))),
            ..StoreIo::default()
        };
        assert!(matches!(s.replace(&[], &io), Err(SaveError::Deleted(_))));
        assert!(
            !s.path.exists(),
            "a stale store must not survive a failed rewrite"
        );
    }

    #[test]
    fn a_failed_rewrite_and_delete_reports_a_stale_store() {
        let (_d, s) = store();
        s.save(&[one()]).expect("save");
        let io = StoreIo {
            write: Arc::new(|_, _| Err(io::Error::other("disk full"))),
            remove: Arc::new(|_| Err(io::Error::other("read-only"))),
        };
        assert!(matches!(s.replace(&[], &io), Err(SaveError::Stale { .. })));
        assert!(s.path.exists(), "the old store is still on disk");
    }
}
