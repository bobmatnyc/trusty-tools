//! The pool of persistent per-slot build directories (#8261).
//!
//! Why: one shared `CARGO_TARGET_DIR` per repo (#6868) bought the warm cache —
//! ~200 s cold, 103 s warm, 17 s warm again — and paid for it with Cargo's own
//! build-directory lock, which serialises every concurrent build sharing the
//! directory. The 2026-09-17 and 2026-09-19 incidents are what that costs: a
//! `cargo check` that spent 40 of 40m44s blocked on the lock, gates running two
//! to three hours, a sibling worktree's rlib clobbering another's so a
//! verification run reported someone else's test binary as its own verdict. A
//! POOL keeps the warm cache and removes the shared lock: each leased builder
//! compiles into a directory only it holds.
//!
//! What: a slot is one directory `<root>/<owner>/<repo>/slot-<n>/` plus the
//! admission token the daemon grants beside it. [`SlotPool::acquire_path`]
//! creates a slot directory on FIRST lease and seeds it once by APFS clone
//! (`cp -c`) from the repo's current shared target directory, so a new slot
//! starts warm instead of paying the cold build. The pool grows LAZILY — a
//! machine that never reaches its ceiling never pays for the slots it did not
//! use — and a released slot KEEPS its directory, which is the warm cache the
//! next holder inherits.
//!
//! **Clone failure is not slot failure.** A filesystem without copy-on-write
//! clones (any non-APFS volume, and every Linux filesystem `cp -c` does not
//! know) still gets a working slot — an empty, cold directory — and the lease
//! record says so through [`SeedKind::ColdDirectory`], because an operator
//! debugging a slow first build needs to know the clone did not happen.
//!
//! Nothing here decides ADMISSION. The count comes from
//! [`builder_capacity`](crate::core::builder_capacity); this module only turns
//! a granted slot index into a directory.
//!
//! Test: the `#[cfg(test)]` suite below, which uses a temp root throughout —
//! #8311 is the 42,000-directory leak from tests that wrote under the real home.

use std::path::{Path, PathBuf};

use trusty_common::github_path::GithubPath;

/// The marker file that records a slot directory has been seeded.
///
/// Why: seeding must happen ONCE per slot, not on every daemon restart and not
/// on every N-shrink-then-grow cycle — a re-clone over a directory holding a
/// live build's artifacts would corrupt it. A marker file survives both, which
/// an in-memory flag does not.
/// What: written inside the slot directory after a successful seed; its
/// presence is the whole test.
/// Test: `a_second_acquire_does_not_reseed`.
pub const SEED_MARKER: &str = ".trusty-slot-seeded";

/// How a slot directory came to exist.
///
/// Why: the lease record must say whether the slot started warm, because that
/// is the difference between a 17-second first gate and a 200-second one, and
/// an operator seeing the slow one needs to know which happened.
/// What: one variant per outcome of the one-time seed.
/// Test: `a_fresh_slot_is_cloned_from_the_shared_directory`,
/// `an_unsupported_clone_still_yields_a_usable_cold_slot`,
/// `a_second_acquire_does_not_reseed`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SeedKind {
    /// Copy-on-write cloned from the repo's shared target directory.
    ClonedFromShared,
    /// Created empty: no clone source, or the filesystem does not support
    /// `cp -c`. The first build in this slot will be cold. `detail` says which.
    ColdDirectory(String),
    /// The directory already existed and carries [`SEED_MARKER`] — the warm
    /// case a released-then-reacquired slot takes.
    AlreadySeeded,
}

/// Why a slot directory could not be provided.
///
/// Why: a session that cannot be given a slot gets NO slot (fail closed) rather
/// than silently falling back to the shared directory — falling back is exactly
/// the clobbering this module exists to end.
/// What: `thiserror`, because this is library code.
/// Test: `an_unwritable_root_is_an_error_not_a_shared_fallback`.
#[derive(Debug, thiserror::Error)]
pub enum SlotPoolError {
    /// The slot directory could not be created.
    #[error("could not create builder slot directory {path}: {source}")]
    Create {
        /// The directory that could not be made.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// One repo's slot pool, rooted at the operator's `builders.slot_pool_root`.
///
/// Why: keyed by `<owner>/<repo>` for the same reason the shared target
/// directory is (#6868) — two checkouts of one repo share a warm cache, and two
/// different repos never contend on one cargo lock. A slot index means nothing
/// across repos, so the repo is part of the pool's identity rather than a
/// parameter on every call.
/// What: holds only paths; creates nothing until [`Self::acquire_path`] is
/// called for a slot index the daemon has actually granted.
/// Test: `slot_paths_are_keyed_by_owner_and_repo`.
#[derive(Debug, Clone)]
pub struct SlotPool {
    root: PathBuf,
    identity: GithubPath,
}

impl SlotPool {
    /// A pool under `root` for one repo.
    ///
    /// Test: `slot_paths_are_keyed_by_owner_and_repo`.
    #[must_use]
    pub fn new(root: PathBuf, identity: GithubPath) -> Self {
        Self { root, identity }
    }

    /// Where slot `index` lives, whether or not it exists yet.
    ///
    /// Why: pure, so a doctor row or a refusal message can name a slot's
    /// directory without creating it. Creating a directory as a side effect of
    /// reporting would make the diagnosis change the thing diagnosed.
    /// What: `<root>/<owner>/<repo>/slot-<index>`.
    /// Test: `slot_paths_are_keyed_by_owner_and_repo`.
    #[must_use]
    pub fn slot_path(&self, index: u32) -> PathBuf {
        self.root
            .join(&self.identity.owner)
            .join(&self.identity.repo)
            .join(format!("slot-{index}"))
    }

    /// Create slot `index` if it does not exist, seeding it once.
    ///
    /// Why: LAZY growth is the owner's 2026-09-20 ruling — the pool is not
    /// pre-sized to the ceiling, so a machine that never reaches its ceiling
    /// never pays that disk. Seeding is one-time because a re-clone over a live
    /// build's artifacts would corrupt them; [`SEED_MARKER`] is what makes it
    /// one-time across daemon restarts.
    /// What: returns the slot's path and how it came to be. An existing,
    /// marked directory is returned untouched — that is the warm cache a
    /// released slot leaves for its next holder. A clone that fails for ANY
    /// reason still yields a usable cold directory; only a directory that
    /// cannot be created at all is an error.
    ///
    /// `clone_from` is the repo's current shared `CARGO_TARGET_DIR`. `None`, or
    /// a path that does not exist, gives a cold slot rather than an error — a
    /// machine with no warm directory yet is an ordinary first run.
    ///
    /// # Errors
    ///
    /// [`SlotPoolError::Create`] when the slot directory cannot be made. The
    /// caller must then grant NO slot: falling back to the shared directory is
    /// the clobbering this module exists to end.
    ///
    /// Test: `a_fresh_slot_is_cloned_from_the_shared_directory`,
    /// `an_unsupported_clone_still_yields_a_usable_cold_slot`,
    /// `a_second_acquire_does_not_reseed`,
    /// `a_released_slot_directory_is_reused_by_the_next_holder`,
    /// `an_unwritable_root_is_an_error_not_a_shared_fallback`.
    pub fn acquire_path(
        &self,
        index: u32,
        clone_from: Option<&Path>,
    ) -> Result<(PathBuf, SeedKind), SlotPoolError> {
        let path = self.slot_path(index);
        if path.join(SEED_MARKER).is_file() {
            return Ok((path, SeedKind::AlreadySeeded));
        }
        let seed = match clone_from.filter(|src| src.is_dir()) {
            Some(src) => match clone_directory(src, &path) {
                Ok(()) => SeedKind::ClonedFromShared,
                Err(detail) => {
                    create_cold(&path)?;
                    SeedKind::ColdDirectory(detail)
                }
            },
            None => {
                create_cold(&path)?;
                SeedKind::ColdDirectory(
                    "no warm shared target directory to clone from".to_string(),
                )
            }
        };
        // The marker goes down last, so a seed interrupted partway is retried
        // rather than inherited as a half-cloned directory.
        if let Err(err) = std::fs::write(path.join(SEED_MARKER), seed_marker_body(&seed)) {
            tracing::warn!(
                "builder slot {} was seeded but its marker could not be written: {err}",
                path.display()
            );
        }
        Ok((path, seed))
    }
}

/// What the marker file records, for a human reading the pool directory.
fn seed_marker_body(seed: &SeedKind) -> String {
    format!("#8261 builder slot pool\nseed: {seed:?}\n")
}

/// Create an empty slot directory, parents included.
fn create_cold(path: &Path) -> Result<(), SlotPoolError> {
    std::fs::create_dir_all(path).map_err(|source| SlotPoolError::Create {
        path: path.to_path_buf(),
        source,
    })
}

/// Copy-on-write clone `src` to `dst`, or say why not.
///
/// Why: `cp -c` is the only portable way to ask APFS for a clone from Rust
/// without an `fcntl`/`clonefile` binding, and the cost matters: a clone of the
/// 207 GB shared directory is near-zero disk and seconds of wall clock, against
/// ~200 s for the cold build it replaces.
/// What: `cp -c -R` on macOS. Any non-zero exit, any missing `cp`, and every
/// non-macOS platform return `Err(detail)` — the caller then creates a cold
/// directory, because a slot that works slowly beats no slot at all.
///
/// # Errors
///
/// A human-readable reason the clone did not happen, for [`SeedKind::ColdDirectory`].
///
/// Test: `an_unsupported_clone_still_yields_a_usable_cold_slot`, and
/// `a_fresh_slot_is_cloned_from_the_shared_directory` on macOS.
fn clone_directory(src: &Path, dst: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err(format!(
            "copy-on-write clone is macOS/APFS only; this is {}",
            std::env::consts::OS
        ));
    }
    if let Some(parent) = dst.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return Err(format!("could not create {}: {err}", parent.display()));
    }
    // `cp -c` fails outright rather than falling back to a full byte copy when
    // the volume cannot clone, which is the behaviour wanted here: a silent
    // 207 GB real copy would fill the disk this design is trying to conserve.
    let output = std::process::Command::new("cp")
        .arg("-c")
        .arg("-R")
        .arg(src)
        .arg(dst)
        .output()
        .map_err(|err| format!("could not run cp: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "cp -c exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> GithubPath {
        GithubPath {
            owner: "bobmatnyc".to_string(),
            repo: "trusty-tools".to_string(),
        }
    }

    /// Every test here roots the pool in a temp dir. #8311: tests that wrote
    /// under the real `~/.trusty-tools` leaked 42,000 directories.
    fn pool(root: &Path) -> SlotPool {
        SlotPool::new(root.to_path_buf(), identity())
    }

    #[test]
    fn slot_paths_are_keyed_by_owner_and_repo() {
        let pool = SlotPool::new(PathBuf::from("/pool"), identity());
        assert_eq!(
            pool.slot_path(3),
            Path::new("/pool/bobmatnyc/trusty-tools/slot-3")
        );
        // Nothing was created by asking.
        assert!(!Path::new("/pool/bobmatnyc").exists());
    }

    #[test]
    fn a_fresh_slot_is_cloned_from_the_shared_directory() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(shared.join("debug")).expect("a warm shared dir");
        std::fs::write(shared.join("debug/libfoo.rlib"), b"warm").expect("an artifact");

        let (path, seed) = pool(&tmp.path().join("pool"))
            .acquire_path(0, Some(&shared))
            .expect("a slot on a writable root");

        assert!(path.is_dir(), "the slot directory exists");
        if cfg!(target_os = "macos") {
            assert_eq!(seed, SeedKind::ClonedFromShared, "APFS clones");
            assert_eq!(
                std::fs::read(path.join("debug/libfoo.rlib")).expect("the cloned artifact"),
                b"warm",
                "the slot starts warm — that is the whole point of seeding"
            );
        } else {
            // Non-APFS platforms take the documented cold path, and still get a
            // usable slot.
            assert!(matches!(seed, SeedKind::ColdDirectory(_)), "{seed:?}");
        }
    }

    #[test]
    fn an_unsupported_clone_still_yields_a_usable_cold_slot() {
        let tmp = tempfile::tempdir().expect("temp root");
        // A clone source that does not exist is the "no warm directory yet"
        // case: a cold slot, never an error.
        let (path, seed) = pool(&tmp.path().join("pool"))
            .acquire_path(1, Some(Path::new("/nonexistent/shared")))
            .expect("a missing clone source is not a slot failure");
        assert!(path.is_dir(), "a cold slot is still a usable slot");
        assert!(matches!(seed, SeedKind::ColdDirectory(_)), "{seed:?}");
    }

    #[test]
    fn a_slot_with_no_clone_source_says_so() {
        let tmp = tempfile::tempdir().expect("temp root");
        let (_, seed) = pool(&tmp.path().join("pool"))
            .acquire_path(0, None)
            .expect("a slot with no source");
        match seed {
            SeedKind::ColdDirectory(detail) => {
                assert!(detail.contains("no warm shared target directory"), "{detail}");
            }
            other => panic!("expected ColdDirectory, got {other:?}"),
        }
    }

    #[test]
    fn a_second_acquire_does_not_reseed() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a shared dir");
        let pool = pool(&tmp.path().join("pool"));

        let (path, _) = pool.acquire_path(0, Some(&shared)).expect("first lease");
        // An artifact the live build wrote. A re-clone would destroy it.
        std::fs::write(path.join("in-progress"), b"mine").expect("a build artifact");

        let (again, seed) = pool.acquire_path(0, Some(&shared)).expect("second lease");
        assert_eq!(again, path);
        assert_eq!(seed, SeedKind::AlreadySeeded);
        assert_eq!(
            std::fs::read(path.join("in-progress")).expect("the artifact survived"),
            b"mine",
        );
    }

    #[test]
    fn a_released_slot_directory_is_reused_by_the_next_holder() {
        let tmp = tempfile::tempdir().expect("temp root");
        let pool = pool(&tmp.path().join("pool"));
        let (first, _) = pool.acquire_path(2, None).expect("holder one");
        std::fs::write(first.join("warm.rlib"), b"cache").expect("an artifact");

        // Releasing a lease keeps the directory — it IS the warm cache.
        let (second, seed) = pool.acquire_path(2, None).expect("holder two");
        assert_eq!(second, first);
        assert_eq!(seed, SeedKind::AlreadySeeded);
        assert!(
            second.join("warm.rlib").is_file(),
            "the next holder inherits the previous holder's cache"
        );
    }

    #[test]
    fn an_unwritable_root_is_an_error_not_a_shared_fallback() {
        let tmp = tempfile::tempdir().expect("temp root");
        // A FILE where the pool root must be a directory: create_dir_all fails.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("a blocking file");

        let err = pool(&blocked)
            .acquire_path(0, None)
            .expect_err("a slot that cannot be made must be refused, never swapped for the shared dir");
        assert!(
            matches!(err, SlotPoolError::Create { .. }),
            "expected Create, got {err:?}"
        );
    }

    #[test]
    fn two_slots_are_two_directories() {
        let tmp = tempfile::tempdir().expect("temp root");
        let pool = pool(&tmp.path().join("pool"));
        let (a, _) = pool.acquire_path(0, None).expect("slot 0");
        let (b, _) = pool.acquire_path(1, None).expect("slot 1");
        assert_ne!(a, b, "two concurrent builders never share a directory");
        assert!(a.is_dir() && b.is_dir());
    }
}
