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
//! admission token the daemon grants beside it. The pool grows LAZILY — a
//! machine that never reaches its ceiling never pays for the slots it did not
//! use — and a released slot KEEPS its directory, which is the warm cache the
//! next holder inherits.
//!
//! **Reserving and seeding are two calls, and the split is a hard requirement
//! (#8261 critic round).** [`SlotPool::reserve_path`] is bounded — a marker
//! stat and one `create_dir_all` of the parent — because it runs inside the
//! daemon's claim mutex, and the hook that is waiting on it gives up after 2
//! seconds. [`SlotPool::seed`] is the unbounded half: it APFS-clones (`cp -c`)
//! a target directory that has been measured at 207 GB, which cannot run on
//! that path. The daemon runs it on a blocking task after the answer is sent,
//! so the FIRST claim on a cold slot is admitted without a private directory
//! and the next one finds it seeded.
//!
//! **A platform that cannot clone still gets a slot.** Every non-macOS
//! platform gets a working slot — an empty, cold directory — and the lease
//! record says so through [`SeedKind::ColdDirectory`], because an operator
//! debugging a slow first build needs to know the clone did not happen. A clone
//! that FAILS on macOS is different (#8794): the slot is left unmarked and
//! retried, because a failed replace leaves a tree in an unknown state.
//!
//! **A slot holding a live build is never replaced (#8794).** Seeding checks
//! the cargo `flock`s in the slot and refuses while one is held.
//!
//! Nothing here decides ADMISSION. The count comes from the build lease
//! ([`crate::core::build_lease::admission`], #8261); this module only turns a
//! granted slot index into a directory.
//!
//! Test: the `#[cfg(test)]` suite below, which uses a temp root throughout —
//! #8311 is the 42,000-directory leak from tests that wrote under the real home.

use std::path::{Path, PathBuf};

use trusty_common::github_path::GithubPath;

mod handover;

/// The marker file that records a slot directory has been seeded.
///
/// Why: seeding must happen ONCE per slot, not on every daemon restart and not
/// on every N-shrink-then-grow cycle — a re-clone over a directory holding a
/// live build's artifacts would corrupt it. A marker file survives both, which
/// an in-memory flag does not.
/// What: written inside the slot directory after a SUCCESSFUL seed only, and
/// published by `hard_link` so exactly ONE run ever writes it and no reader
/// sees it half-written. Presence alone is not the test (#8794): the body must
/// read as seeded (`marker_reads_seeded`), which is also what
/// [`SlotPool::reserve_path`] checks to decide whether a slot can be granted now.
/// Test: `a_second_seed_does_not_reseed`,
/// `a_reservation_on_a_seeded_slot_is_ready`,
/// `two_staging_trees_for_one_index_publish_exactly_one_slot`,
/// `a_failure_text_marker_is_treated_as_unseeded`.
pub const SEED_MARKER: &str = ".trusty-slot-seeded";

/// The line a marker body carries when the seed succeeded (#8794).
const SEEDED_STATUS_LINE: &str = "status: seeded";

/// The cold-slot detail when there is no warm directory to clone from.
const NO_SOURCE_DETAIL: &str = "no warm shared target directory to clone from";

/// The cold-slot detail prefix on a platform `cp -c` cannot clone on.
const UNSUPPORTED_PREFIX: &str = "copy-on-write clone is macOS/APFS only";

/// The lock files cargo creates in a build directory (`<target>/<profile>/`).
///
/// Why (#8794): cargo leaves these files behind after every build, so their
/// existence says nothing; only a HELD `flock` on one marks a live build.
/// Probed on cargo 1.94: `debug/.cargo-lock` is held exclusively for the whole
/// build and free afterwards. The other two names are newer build-dir locks.
const CARGO_LOCK_NAMES: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// How a slot directory came to exist.
///
/// Why: the lease record must say whether the slot started warm, because that
/// is the difference between a 17-second first gate and a 200-second one, and
/// an operator seeing the slow one needs to know which happened.
/// What: one variant per outcome of the one-time seed.
/// Test: `a_fresh_slot_is_cloned_from_the_shared_directory`,
/// `a_missing_clone_source_still_yields_a_usable_cold_slot`,
/// `a_second_seed_does_not_reseed`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SeedKind {
    /// Copy-on-write cloned from the repo's shared target directory.
    ClonedFromShared,
    /// Created empty: no clone source, or a platform `cp -c` cannot clone on.
    /// The first build in this slot will be cold. `detail` says which.
    ColdDirectory(String),
    /// The directory already existed and its [`SEED_MARKER`] reads as seeded — the warm
    /// case a released-then-reacquired slot takes.
    AlreadySeeded,
}

/// What one claim-time reservation resolved to (#8261 critic round).
///
/// Why: the claim path may not clone, and it may not hand out a directory that
/// has not been cloned yet — a builder pointed at a half-seeded slot would
/// compile against a partial cache. So the bounded call answers which of the two
/// states the slot is in, and the caller decides.
/// What: [`Self::Ready`] is a slot whose [`SEED_MARKER`] reads as seeded, usable now.
/// [`Self::Seeding`] is a slot whose parent now exists and whose seed has still
/// to run; its path is NOT granted to this claim.
/// Test: `a_reservation_on_an_unseeded_slot_seeds_nothing`,
/// `a_reservation_on_a_seeded_slot_is_ready`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotReservation {
    /// The slot is seeded and can be built in now.
    Ready(PathBuf),
    /// The slot still needs [`SlotPool::seed`], which the caller must run off
    /// its own hot path.
    Seeding(PathBuf),
}

/// Why a slot directory could not be provided.
///
/// Why: a session that cannot be given a slot gets NO slot (fail closed) rather
/// than silently falling back to the shared directory — falling back is exactly
/// the clobbering this module exists to end.
/// What: `thiserror`, because this is library code.
/// Test: `an_unwritable_root_is_an_error_not_a_shared_fallback`.
// #8372: non_exhaustive from its first release, so a new failure is not an API break.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    /// #8794: the slot holds a live cargo build, so the seed or the handover
    /// refused it and left it as it was.
    // #8794: neutral text — a handover refusal is not a seed refusal.
    #[error("builder slot {path} is in use: cargo holds {lock}, so a build is running there")]
    ActiveBuild {
        /// The slot directory left untouched.
        path: PathBuf,
        /// The cargo lock file found held.
        lock: PathBuf,
    },
    /// #8794: the clone did not land and the slot is in an unknown state, so
    /// it stays unseeded (no marker) for the next attempt.
    #[error("could not seed builder slot {path}: {detail}")]
    SeedFailed {
        /// The slot directory that stays unseeded.
        path: PathBuf,
        /// Why the seed failed.
        detail: String,
    },
    /// #8794: a seeded slot could not be handed to a new holder, so it is not
    /// handed out.
    #[error("could not hand builder slot {path} to a new holder: {detail}")]
    HandOver {
        /// The slot directory that was not handed out.
        path: PathBuf,
        /// Why the handover failed.
        detail: String,
    },
}

/// One repo's slot pool, rooted at the operator's `builders.slot_pool_root`.
///
/// Why: keyed by `<owner>/<repo>` for the same reason the shared target
/// directory is (#6868) — two checkouts of one repo share a warm cache, and two
/// different repos never contend on one cargo lock. A slot index means nothing
/// across repos, so the repo is part of the pool's identity rather than a
/// parameter on every call.
/// What: holds only paths; creates nothing until [`Self::reserve_path`] is
/// called for a slot index the daemon has actually granted, and nothing beyond
/// that slot's parent until [`Self::seed`] runs off the claim path.
/// Test: `slot_paths_are_keyed_by_owner_and_repo`.
#[derive(Debug, Clone)]
pub struct SlotPool {
    root: PathBuf,
    identity: GithubPath,
    /// Where the dispatch was issued from; see [`Self::with_checkout`].
    checkout: Option<PathBuf>,
    /// #8794: slots `0..ceiling` are the only ones a claim may start seeding.
    ceiling: u32,
}

impl SlotPool {
    /// A pool under `root` for one repo, seeding at most `ceiling` slots.
    ///
    /// Why (#8794): the seed of a new slot clones a target directory measured
    /// at 207 GB. Without a bound, a pool whose seeded slots all fail their
    /// handover would clone a new `slot-N` on every admission. `ceiling` is the
    /// operator's builder ceiling, floored at one so a slot can always exist.
    /// #8261: the build lease passes its own slot count, and holds no index at
    /// or above it.
    /// Test: `slot_paths_are_keyed_by_owner_and_repo`.
    #[must_use]
    pub fn new(root: PathBuf, identity: GithubPath, ceiling: u32) -> Self {
        Self {
            root,
            identity,
            checkout: None,
            ceiling: ceiling.max(1),
        }
    }

    /// The number of slot indexes, from zero, a claim may start seeding.
    #[must_use]
    pub fn ceiling(&self) -> u32 {
        self.ceiling
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

    /// Is slot `index` usable right now, and if not, prepare for its seed.
    ///
    /// Why: this is the only half of the pool a claim may run. It is BOUNDED —
    /// one stat and one `create_dir_all` of the parent — because it executes
    /// inside the daemon's claim mutex while a hook waits on a 2-second budget.
    /// The clone that is not bounded lives in [`Self::seed`]. Creating the
    /// parent here is not incidental: it is still the fail-closed gate, so a
    /// pool root that cannot be written refuses the claim at claim time rather
    /// than minutes later on a background task nobody is reading.
    /// What: [`SlotReservation::Ready`] when [`SEED_MARKER`] reads as seeded — the
    /// warm case a released slot leaves for its next holder — else
    /// [`SlotReservation::Seeding`], which grants NO directory and obliges the
    /// caller to run [`Self::seed`] off its own hot path.
    ///
    /// # Errors
    ///
    /// [`SlotPoolError::Create`] when the slot's parent cannot be made. The
    /// caller must then grant NO slot: falling back to the shared directory is
    /// the clobbering this module exists to end.
    ///
    /// Test: `a_reservation_on_an_unseeded_slot_seeds_nothing`,
    /// `a_reservation_on_a_seeded_slot_is_ready`,
    /// `an_unwritable_root_is_an_error_not_a_shared_fallback`.
    pub fn reserve_path(&self, index: u32) -> Result<SlotReservation, SlotPoolError> {
        let path = self.slot_path(index);
        // #8794: a marker a failed seed wrote does not make the slot Ready.
        if marker_reads_seeded(&path) {
            return Ok(SlotReservation::Ready(path));
        }
        let parent = path.parent().unwrap_or(path.as_path());
        std::fs::create_dir_all(parent).map_err(|source| SlotPoolError::Create {
            path: parent.to_path_buf(),
            source,
        })?;
        Ok(SlotReservation::Seeding(path))
    }

    /// Create slot `index` if it does not exist, seeding it once.
    ///
    /// Why: LAZY growth is the owner's 2026-09-20 ruling — the pool is not
    /// pre-sized to the ceiling, so a machine that never reaches its ceiling
    /// never pays that disk. Seeding is one-time because a re-clone over a live
    /// build's artifacts would corrupt them; [`SEED_MARKER`] is what makes it
    /// one-time across daemon restarts. This is the UNBOUNDED half of the pool
    /// and must never run on a path a hook is waiting on — see the module doc.
    /// What: returns the slot's path and how it came to be. A directory whose
    /// marker reads as seeded is returned untouched. A slot where a cargo build
    /// holds its lock is refused and left as it is (#8794). A clone the
    /// platform cannot make yields a usable cold directory; a clone that fails
    /// on a clone-capable platform writes NO marker, records the failure beside
    /// the slot (`.slot-<n>.seed-failed`), and leaves the slot unseeded (#8794).
    /// Every attempt and its outcome is logged at info/warn.
    ///
    /// `clone_from` is the repo's current shared `CARGO_TARGET_DIR`. `None`, or
    /// a path that does not exist, gives a cold slot rather than an error — a
    /// machine with no warm directory yet is an ordinary first run.
    ///
    /// # Errors
    ///
    /// [`SlotPoolError::Create`] when the slot directory cannot be made, or when
    /// [`SEED_MARKER`] cannot be written. The marker failure is an error and not
    /// a warning (#8261 critic round): an unmarked directory is re-seeded by the
    /// next caller. [`SlotPoolError::ActiveBuild`] when a live build holds the
    /// slot; [`SlotPoolError::SeedFailed`] when the clone did not land.
    ///
    /// Test: `a_fresh_slot_is_cloned_from_the_shared_directory`,
    /// `a_missing_clone_source_still_yields_a_usable_cold_slot`,
    /// `a_second_seed_does_not_reseed`,
    /// `a_released_slot_directory_is_reused_by_the_next_holder`,
    /// `an_unmarked_directory_is_reseeded_without_nesting`,
    /// `an_unwritable_root_is_an_error_not_a_shared_fallback`,
    /// `a_slot_with_a_held_cargo_lock_is_not_replaced`,
    /// `a_stale_unheld_cargo_lock_does_not_block_seeding`,
    /// `a_failed_replace_leaves_no_seeded_marker`,
    /// `a_failure_text_marker_is_treated_as_unseeded`.
    pub fn seed(
        &self,
        index: u32,
        clone_from: Option<&Path>,
    ) -> Result<(PathBuf, SeedKind), SlotPoolError> {
        let path = self.slot_path(index);
        if marker_reads_seeded(&path) {
            return Ok((path, SeedKind::AlreadySeeded));
        }
        // #8794: every replace attempt and its outcome goes to the daemon log,
        // so how often seeding refuses or fails is measurable there.
        let repo = format!("{}/{}", self.identity.owner, self.identity.repo);
        tracing::info!(slot = %path.display(), %repo, "builder slot seed: replace attempt");
        let result = seed_unmarked(&path, clone_from);
        match &result {
            Ok(kind) => {
                tracing::info!(slot = %path.display(), %repo, outcome = "seeded", seed = ?kind, "builder slot seed: outcome");
            }
            Err(err @ SlotPoolError::ActiveBuild { lock, .. }) => {
                tracing::warn!(slot = %path.display(), %repo, outcome = "refused-active-build", lock = %lock.display(), "builder slot seed: outcome");
                // #8794: recorded so the next admission can say why the slot is
                // still unseeded.
                record_seed_failure(&path, &err.to_string());
            }
            Err(err) => {
                tracing::warn!(slot = %path.display(), %repo, outcome = "failed", error = %err, "builder slot seed: outcome");
            }
        }
        result.map(|kind| (path, kind))
    }
}

/// The unseeded half of [`SlotPool::seed`]: guard, clone, mark (#8794).
fn seed_unmarked(path: &Path, clone_from: Option<&Path>) -> Result<SeedKind, SlotPoolError> {
    // #8794: an unseeded slot can still hold a live build — a builder given
    // this directory by hand, or one admitted while the seed was pending. It is
    // refused before the clone as well as before the replace, so a refusal
    // costs no clone.
    refuse_active_build(path)?;
    retire_unseeded_marker(path);
    sweep_abandoned_staging(path);
    let seed = match clone_from.filter(|src| src.is_dir()) {
        Some(src) => match clone_directory(src, path) {
            Ok(()) => SeedKind::ClonedFromShared,
            // A clone that lost the publish race to another run: the warm case.
            Err(CloneError::Published) => return Ok(SeedKind::AlreadySeeded),
            Err(CloneError::ActiveBuild(lock)) => {
                return Err(SlotPoolError::ActiveBuild {
                    path: path.to_path_buf(),
                    lock,
                });
            }
            Err(CloneError::Unsupported(detail)) => {
                create_cold(path)?;
                SeedKind::ColdDirectory(detail)
            }
            // #8794: no marker, so the slot stays unseeded and is retried.
            Err(CloneError::Failed(detail)) => {
                record_seed_failure(path, &detail);
                return Err(SlotPoolError::SeedFailed {
                    path: path.to_path_buf(),
                    detail,
                });
            }
        },
        None => {
            create_cold(path)?;
            SeedKind::ColdDirectory(NO_SOURCE_DETAIL.to_string())
        }
    };
    publish_marker(path, seed)
}

/// Write [`SEED_MARKER`] for a successful seed, atomically and exactly once.
///
/// Why: the marker goes down last, so a seed interrupted partway is retried
/// rather than inherited as a half-cloned directory, and exactly one run ever
/// marks a slot (#8261 critic round 3). #8794: the body is written to a private
/// file first and `hard_link`ed into place, because a reader now judges the
/// body and must never see an empty, half-written marker.
/// What: `AlreadyExists` on the link is the lost election — the warm case.
/// Test: `two_staging_trees_for_one_index_publish_exactly_one_slot`.
fn publish_marker(path: &Path, seed: SeedKind) -> Result<SeedKind, SlotPoolError> {
    let marker = path.join(SEED_MARKER);
    let draft = path.join(format!(
        "{SEED_MARKER}.draft.{}.{}",
        std::process::id(),
        unique_nanos()
    ));
    let body = format!("#8261 builder slot pool\n{SEEDED_STATUS_LINE}\nseed: {seed:?}\n");
    let created = std::fs::write(&draft, body).map_err(|source| SlotPoolError::Create {
        path: draft.clone(),
        source,
    });
    let linked = created.and_then(|()| {
        std::fs::hard_link(&draft, &marker).map_err(|source| SlotPoolError::Create {
            path: marker.clone(),
            source,
        })
    });
    drop(std::fs::remove_file(&draft));
    match linked {
        Ok(()) => {
            drop(std::fs::remove_file(seed_failure_path(path)));
            Ok(seed)
        }
        Err(SlotPoolError::Create { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            Ok(SeedKind::AlreadySeeded)
        }
        Err(err) => Err(err),
    }
}

/// Does slot `path` carry a marker a SUCCESSFUL seed wrote? (#8794)
///
/// Why: before #8794 a failed clone stamped `ColdDirectory("<error>")` into the
/// marker, and presence alone read as seeded — slot-6 on 2026-09-27 read as
/// seeded over a half-deleted tree.
/// What: a marker is seeded when its body carries [`SEEDED_STATUS_LINE`]. A
/// pre-#8794 body (no status line) is seeded exactly when the current code
/// would have written it: `ClonedFromShared`, or a cold slot with no clone
/// source or no clone support. Any other legacy body — a clone or replace
/// failure — and an empty or unreadable marker read as unseeded.
/// Test: `legacy_markers_read_as_seeded_only_when_the_seed_succeeded`,
/// `a_failure_text_marker_is_treated_as_unseeded`.
fn marker_reads_seeded(path: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(path.join(SEED_MARKER)) else {
        return false;
    };
    body_reads_seeded(&body)
}

/// The body test behind [`marker_reads_seeded`].
fn body_reads_seeded(body: &str) -> bool {
    if body.lines().any(|line| line == SEEDED_STATUS_LINE) {
        return true;
    }
    body.lines().any(|line| {
        line == "seed: ClonedFromShared"
            || line == format!("seed: ColdDirectory({NO_SOURCE_DETAIL:?})")
            || line.starts_with(&format!("seed: ColdDirectory(\"{UNSUPPORTED_PREFIX}"))
    })
}

/// Where a failed seed of slot `path` is recorded: `<parent>/.<slot>.seed-failed`.
///
/// Why (#8794): beside the slot, not inside it, so it survives the slot being
/// replaced and never reads as a marker; a successful seed removes it.
fn seed_failure_path(path: &Path) -> PathBuf {
    let slot = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("slot");
    path.with_file_name(format!("{STAGING_PREFIX_DOT}{slot}.seed-failed"))
}

/// Record a failed seed beside the slot. Best effort — the log has it too.
fn record_seed_failure(path: &Path, detail: &str) {
    let body = format!("#8794 builder slot seed failed\nerror: {detail}\n");
    if let Err(err) = std::fs::write(seed_failure_path(path), body) {
        tracing::warn!(slot = %path.display(), "could not record the failed seed: {err}");
    }
}

/// Move a marker that does not read as seeded out of the slot (#8794).
///
/// Why: a pre-#8794 failed seed left `ColdDirectory("<error>")` in the marker.
/// Left in place it would win the `hard_link` election against this run's real
/// marker. Its text moves to [`seed_failure_path`], where a failure belongs.
fn retire_unseeded_marker(path: &Path) {
    let marker = path.join(SEED_MARKER);
    if !marker.is_file() || marker_reads_seeded(path) {
        return;
    }
    let body = std::fs::read_to_string(&marker).unwrap_or_default();
    tracing::info!(slot = %path.display(), marker = %body.trim(), "builder slot seed: retiring a marker that does not read as seeded");
    if let Err(err) = std::fs::rename(&marker, seed_failure_path(path)) {
        tracing::warn!(slot = %path.display(), "could not retire the unseeded marker: {err}");
    }
}

/// Refuse when a cargo build holds a lock in slot `path` (#8794).
fn refuse_active_build(path: &Path) -> Result<(), SlotPoolError> {
    match hold_idle_build_locks(path) {
        Ok(_released_on_drop) => Ok(()),
        Err(lock) => Err(SlotPoolError::ActiveBuild {
            path: path.to_path_buf(),
            lock,
        }),
    }
}

/// Take every cargo lock in slot `path`, or name the one a live build holds.
///
/// Why (#8794): cargo leaves its lock files behind after a build, so file
/// existence cannot tell a live build from a finished one — the `flock` can.
/// Taking the lock (rather than peeking and releasing) lets the caller keep
/// it across the replace, so a cargo that starts mid-replace blocks on it
/// instead of writing into a tree being deleted.
/// What: looks for [`CARGO_LOCK_NAMES`] in the slot, each child directory
/// (`debug/`, `release/`) and each grandchild (`<triple>/<profile>/`), and
/// `try_lock`s each one found. `Ok` holds every lock until dropped. `Err` names
/// a lock that is held — or one whose state cannot be read, since destroying
/// a live build is the outcome this guard exists to rule out (ADR-0045).
/// Test: `a_slot_with_a_held_cargo_lock_is_not_replaced`,
/// `a_stale_unheld_cargo_lock_does_not_block_seeding`.
fn hold_idle_build_locks(path: &Path) -> Result<Vec<std::fs::File>, PathBuf> {
    let mut dirs = vec![path.to_path_buf()];
    for child in subdirectories(path) {
        dirs.extend(subdirectories(&child));
        dirs.push(child);
    }
    let mut held = Vec::new();
    for lock in dirs
        .iter()
        .flat_map(|dir| CARGO_LOCK_NAMES.iter().map(|name| dir.join(name)))
    {
        let file = match std::fs::File::open(&lock) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(lock),
        };
        match file.try_lock() {
            Ok(()) => held.push(file),
            // A filesystem with no `flock` support: cargo skips locking there
            // too, so no lock can be held and there is nothing to read.
            Err(std::fs::TryLockError::Error(err))
                if err.kind() == std::io::ErrorKind::Unsupported => {}
            Err(_) => return Err(lock),
        }
    }
    Ok(held)
}

/// The real (non-symlink) subdirectories of `dir`; none when it cannot be read.
fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// Nanoseconds since the epoch, for per-run unique file names.
fn unique_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// This run's private staging directory for `dst`.
///
/// Why: a fixed staging name is a shared mutable directory between any two seed
/// runs, and the first act of a seed is to clear it — so the later run deletes
/// the earlier one's tree mid-copy. The in-daemon registry cannot prevent that
/// pairing across a restart: `std::process::Command` sets no death signal (macOS
/// has none), so a `cp -c -R` outlives the daemon that spawned it while the new
/// daemon's registry starts empty (#8261 critic round 3). PID plus nanoseconds
/// is unique across both processes and repeated seeds within one.
/// What: `<parent>/.<slot>.seeding.<pid>.<nanos>`.
/// Test: `two_staging_trees_for_one_index_publish_exactly_one_slot`.
fn staging_path(parent: &Path, slot: &str) -> PathBuf {
    parent.join(format!(
        "{STAGING_PREFIX_DOT}{slot}.seeding.{}.{}",
        std::process::id(),
        unique_nanos()
    ))
}

/// The leading character every staging directory's name carries.
const STAGING_PREFIX_DOT: &str = ".";

/// Delete staging trees left by seed runs whose process is gone (#8261).
///
/// Why: a staging tree is a full clone of the shared target directory, so an
/// abandoned one is real disk — and abandoning one is now possible, because a
/// `cp` that outlives its daemon can be killed before it publishes. Liveness is
/// the only safe test: deleting a LIVE run's staging tree would restore exactly
/// the two-writer corruption the unique name removes.
/// What: scans the slot's parent for `.<slot>.seeding.<pid>.<nanos>` entries and
/// removes only those whose `<pid>` is confirmed dead. An unparseable name, or a
/// pid that is alive or undeterminable, is left alone (ADR-0045).
/// Test: `two_staging_trees_for_one_index_publish_exactly_one_slot`.
fn sweep_abandoned_staging(dst: &Path) {
    let (Some(parent), Some(slot)) = (
        dst.parent(),
        dst.file_name().and_then(std::ffi::OsStr::to_str),
    ) else {
        return;
    };
    let prefix = format!("{STAGING_PREFIX_DOT}{slot}.seeding.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else {
            continue;
        };
        let Ok(pid) = rest.split('.').next().unwrap_or_default().parse::<u32>() else {
            continue;
        };
        if crate::core::process::is_process_alive(pid) {
            continue;
        }
        if let Err(err) = std::fs::remove_dir_all(entry.path()) {
            tracing::warn!(
                "could not sweep abandoned builder-slot staging {}: {err}",
                entry.path().display()
            );
        }
    }
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
/// What: `cp -c -R` on macOS, into a staging sibling that is then renamed into
/// place. Any non-zero exit, any missing `cp`, and every non-macOS platform
/// return `Err(detail)` — the caller then creates a cold directory, because a
/// slot that works slowly beats no slot at all.
///
/// **Never `cp -c -R <src> <dst>` onto an existing `dst`.** BSD `cp` then writes
/// `dst/<basename(src)>` and still exits 0, so the slot would be reported
/// [`SeedKind::ClonedFromShared`] with an empty top level and a nested copy
/// underneath — a cold build that claims to be warm (#8261 critic round). The
/// staging tree has no such case, and the rename is atomic.
///
/// **The staging tree is private to this run** ([`staging_path`]), and `dst` is
/// replaced only while it carries no [`SEED_MARKER`]. Together those two make a
/// second run — one that survived a daemon restart, which the in-memory registry
/// cannot see — unable to delete this run's tree or to overwrite a slot a
/// builder was granted (#8261 critic round 3).
///
/// **A live build's directory is never replaced (#8794).** The cargo locks in
/// `dst` are taken ([`hold_idle_build_locks`]) right before the replace and
/// held until the clone is in place; a held one aborts with
/// [`CloneError::ActiveBuild`] and `dst` is left as it was.
///
/// # Errors
///
/// [`CloneError`], which tells the caller whether a cold slot, a refusal, or an
/// unseeded retry is the right outcome.
///
/// Test: `a_missing_clone_source_still_yields_a_usable_cold_slot`,
/// `two_staging_trees_for_one_index_publish_exactly_one_slot`, and
/// `a_fresh_slot_is_cloned_from_the_shared_directory`,
/// `an_unmarked_directory_is_reseeded_without_nesting`,
/// `a_slot_with_a_held_cargo_lock_is_not_replaced`,
/// `a_failed_replace_leaves_no_seeded_marker` on macOS.
fn clone_directory(src: &Path, dst: &Path) -> Result<(), CloneError> {
    if !cfg!(target_os = "macos") {
        return Err(CloneError::Unsupported(format!(
            "{UNSUPPORTED_PREFIX}; this is {}",
            std::env::consts::OS
        )));
    }
    let Some(parent) = dst.parent() else {
        return Err(CloneError::Failed(format!(
            "{} has no parent directory",
            dst.display()
        )));
    };
    if let Err(err) = std::fs::create_dir_all(parent) {
        return Err(CloneError::Failed(format!(
            "could not create {}: {err}",
            parent.display()
        )));
    }
    let slot = dst
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("slot");
    let staging = staging_path(parent, slot);
    // `cp -c` fails outright rather than falling back to a full byte copy when
    // the volume cannot clone, which is the behaviour wanted here: a silent
    // 207 GB real copy would fill the disk this design is trying to conserve.
    let output = std::process::Command::new("cp")
        .arg("-c")
        .arg("-R")
        .arg(src)
        .arg(&staging)
        .output()
        .map_err(|err| CloneError::Failed(format!("could not run cp: {err}")))?;
    if !output.status.success() {
        drop(std::fs::remove_dir_all(&staging));
        return Err(CloneError::Failed(format!(
            "cp -c exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    // Re-read the marker here, not only at `seed`'s entry: a run that survived
    // a daemon restart can have published this slot while `cp` was running, and
    // replacing a MARKED directory would destroy a tree a builder holds.
    if marker_reads_seeded(dst) {
        drop(std::fs::remove_dir_all(&staging));
        return Err(CloneError::Published);
    }
    // #8794: `cp` can run for minutes, so a build that started meanwhile is
    // caught here, and the locks stay held until the clone is in place.
    let _locks = match hold_idle_build_locks(dst) {
        Ok(locks) => locks,
        Err(lock) => {
            drop(std::fs::remove_dir_all(&staging));
            return Err(CloneError::ActiveBuild(lock));
        }
    };
    if dst.exists()
        && let Err(err) = std::fs::remove_dir_all(dst)
    {
        drop(std::fs::remove_dir_all(&staging));
        return Err(CloneError::Failed(format!(
            "could not replace the unseeded {}: {err}",
            dst.display()
        )));
    }
    std::fs::rename(&staging, dst).map_err(|err| {
        drop(std::fs::remove_dir_all(&staging));
        CloneError::Failed(format!(
            "could not move the clone into {}: {err}",
            dst.display()
        ))
    })
}

/// Why a clone did not land, sorted by what the seed should do next (#8794).
#[derive(Debug)]
enum CloneError {
    /// This platform cannot clone at all: a cold slot is the documented outcome.
    Unsupported(String),
    /// Another run published the slot first: the warm case.
    Published,
    /// A live build holds this lock in the slot: refuse, touch nothing.
    ActiveBuild(PathBuf),
    /// The clone or the replace failed: no marker, retry next time.
    Failed(String),
}

/// Fixtures shared by every suite that hands a pool slot over (#8794).
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    /// Make `dir` a checkout whose `Cargo.lock` names one path package
    /// (`widgets-core`) and one registry package (`serde`), and return it. A
    /// handover refuses a pool with no resolvable path package (#8794).
    pub(crate) fn write_checkout(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).expect("checkout");
        std::fs::write(
            dir.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"widgets-core\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .expect("Cargo.lock");
        dir.to_path_buf()
    }
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
        SlotPool::new(root.to_path_buf(), identity(), 8)
    }

    #[test]
    fn slot_paths_are_keyed_by_owner_and_repo() {
        let pool = SlotPool::new(PathBuf::from("/pool"), identity(), 0);
        assert_eq!(pool.ceiling(), 1, "a pool can always hold one slot");
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
            .seed(0, Some(&shared))
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
    fn a_missing_clone_source_still_yields_a_usable_cold_slot() {
        let tmp = tempfile::tempdir().expect("temp root");
        // A clone source that does not exist is the "no warm directory yet"
        // case: a cold slot, never an error.
        let (path, seed) = pool(&tmp.path().join("pool"))
            .seed(1, Some(Path::new("/nonexistent/shared")))
            .expect("a missing clone source is not a slot failure");
        assert!(path.is_dir(), "a cold slot is still a usable slot");
        assert!(matches!(seed, SeedKind::ColdDirectory(_)), "{seed:?}");
    }

    /// #8261 critic round: the claim path may only RESERVE. A reservation on an
    /// unseeded slot must create nothing but the parent and grant no directory,
    /// or the 207 GB clone runs under the daemon's claim mutex against a hook
    /// that gives up after 2 seconds.
    #[test]
    fn a_reservation_on_an_unseeded_slot_seeds_nothing() {
        let tmp = tempfile::tempdir().expect("temp root");
        let root = tmp.path().join("pool");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a shared dir");
        std::fs::write(shared.join("sentinel"), b"warm").expect("a sentinel artifact");

        let reserved = pool(&root).reserve_path(0).expect("a writable root");

        let SlotReservation::Seeding(path) = reserved else {
            panic!("an unseeded slot reserves as Seeding, got {reserved:?}");
        };
        assert!(
            !path.exists(),
            "the slot directory itself is the seed's to make: {path:?}"
        );
        assert!(
            path.parent().is_some_and(Path::is_dir),
            "the parent IS created — it is the fail-closed gate"
        );
        assert!(
            !path.join("sentinel").exists(),
            "nothing may be cloned on the claim path"
        );
    }

    /// The warm case: a marked slot reserves as `Ready` and is handed straight
    /// to the builder.
    #[test]
    fn a_reservation_on_a_seeded_slot_is_ready() {
        let tmp = tempfile::tempdir().expect("temp root");
        let pool = pool(&tmp.path().join("pool"));
        let (seeded, _) = pool.seed(0, None).expect("a cold slot");

        assert_eq!(
            pool.reserve_path(0).expect("a writable root"),
            SlotReservation::Ready(seeded),
            "a marked slot is usable now"
        );
    }

    /// #8261 critic round: `cp -c -R src dst` onto an existing `dst` nests the
    /// copy under `dst/<basename(src)>` and still exits 0. An unmarked
    /// directory — the state a failed marker write used to leave behind — must
    /// therefore be re-seeded flat, never nested.
    #[test]
    fn an_unmarked_directory_is_reseeded_without_nesting() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a shared dir");
        std::fs::write(shared.join("sentinel"), b"warm").expect("an artifact");
        let pool = pool(&tmp.path().join("pool"));

        // An UNMARKED slot directory, as an interrupted seed leaves one.
        let slot = pool.slot_path(0);
        std::fs::create_dir_all(&slot).expect("a half-made slot");

        let (path, seed) = pool.seed(0, Some(&shared)).expect("the re-seed succeeds");

        assert!(
            !path.join("shared").exists(),
            "the clone must not nest under the slot: {path:?}"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(seed, SeedKind::ClonedFromShared);
            assert!(
                path.join("sentinel").is_file(),
                "a ClonedFromShared slot must actually carry the cache"
            );
        }
        assert!(
            path.join(SEED_MARKER).is_file(),
            "the re-seed marks the slot, or the next caller re-seeds it again"
        );
    }

    #[test]
    fn a_slot_with_no_clone_source_says_so() {
        let tmp = tempfile::tempdir().expect("temp root");
        let (_, seed) = pool(&tmp.path().join("pool"))
            .seed(0, None)
            .expect("a slot with no source");
        match seed {
            SeedKind::ColdDirectory(detail) => {
                assert!(
                    detail.contains("no warm shared target directory"),
                    "{detail}"
                );
            }
            other => panic!("expected ColdDirectory, got {other:?}"),
        }
    }

    #[test]
    fn a_second_seed_does_not_reseed() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a shared dir");
        let pool = pool(&tmp.path().join("pool"));

        let (path, _) = pool.seed(0, Some(&shared)).expect("first lease");
        // An artifact the live build wrote. A re-clone would destroy it.
        std::fs::write(path.join("in-progress"), b"mine").expect("a build artifact");

        let (again, seed) = pool.seed(0, Some(&shared)).expect("second lease");
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
        let (first, _) = pool.seed(2, None).expect("holder one");
        std::fs::write(first.join("warm.rlib"), b"cache").expect("an artifact");

        // Releasing a lease keeps the directory — it IS the warm cache.
        let (second, seed) = pool.seed(2, None).expect("holder two");
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

        // The RESERVATION is the fail-closed gate, so it is what must refuse —
        // by the time `seed` runs, the claim has already been answered.
        let err = pool(&blocked).reserve_path(0).expect_err(
            "a slot that cannot be made must be refused, never swapped for the shared dir",
        );
        assert!(
            matches!(err, SlotPoolError::Create { .. }),
            "expected Create, got {err:?}"
        );
    }

    /// #8261 critic round 3, MEDIUM: two seed runs never share a staging tree.
    ///
    /// The in-daemon registry cannot pair with a `cp -c -R` that outlived the
    /// daemon that spawned it — `std::process::Command` sets no death signal,
    /// and macOS has none — so the new daemon's empty registry would spawn a
    /// second seed whose first act, under the old fixed `.slot-N.seeding` name,
    /// was `remove_dir_all` of the survivor's tree. Staging names are now
    /// per-run, only a dead run's tree is swept, and `create_new` makes the
    /// marker write the single election.
    #[test]
    fn two_staging_trees_for_one_index_publish_exactly_one_slot() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(&shared).expect("a shared dir");
        std::fs::write(shared.join("sentinel"), b"warm").expect("an artifact");
        let pool = pool(&tmp.path().join("pool"));
        let slot = pool.slot_path(0);
        let parent = slot.parent().expect("a slot has a parent").to_path_buf();
        std::fs::create_dir_all(&parent).expect("the pool parent");

        // A tree left by a run whose process is gone — pid 1 is `launchd` and
        // never dies, so the LIVE case is the one held by a real pid here.
        let abandoned = parent.join(".slot-0.seeding.4294967294.1");
        std::fs::create_dir_all(&abandoned).expect("an abandoned staging tree");
        let live = parent.join(format!(".slot-0.seeding.{}.1", std::process::id()));
        std::fs::create_dir_all(&live).expect("a live run's staging tree");

        let (path, _) = pool.seed(0, Some(&shared)).expect("the seed publishes");

        assert!(
            !abandoned.exists(),
            "a dead run's staging tree is swept: {abandoned:?}"
        );
        assert!(
            live.exists(),
            "a LIVE run's staging tree must never be deleted — that is the \
             two-writer corruption this fix removes"
        );
        assert!(
            path.join(SEED_MARKER).is_file(),
            "the winner marks the slot"
        );
        let staging_left: Vec<_> = std::fs::read_dir(&parent)
            .expect("read the pool parent")
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(".slot-0.seeding.") && !e.path().eq(&live))
            })
            .collect();
        assert!(
            staging_left.is_empty(),
            "the published run leaves no staging tree behind: {staging_left:?}"
        );

        // A second run over the published slot writes no second marker and
        // reports the warm case rather than re-cloning.
        let marked_at = std::fs::metadata(path.join(SEED_MARKER))
            .and_then(|m| m.modified())
            .expect("marker mtime");
        let (again, seed) = pool.seed(0, Some(&shared)).expect("the second run");
        assert_eq!(again, path);
        assert_eq!(seed, SeedKind::AlreadySeeded);
        assert_eq!(
            std::fs::metadata(path.join(SEED_MARKER))
                .and_then(|m| m.modified())
                .expect("marker mtime"),
            marked_at,
            "the marker is written exactly once"
        );
    }

    /// A shared target directory carrying one sentinel artifact.
    fn warm_shared(root: &Path) -> PathBuf {
        let shared = root.join("shared");
        std::fs::create_dir_all(&shared).expect("a shared dir");
        std::fs::write(shared.join("sentinel"), b"warm").expect("an artifact");
        shared
    }

    /// #8794: slot-6 on 2026-09-27 — an unseeded slot a live cargo build was
    /// writing to was deleted by the seed. A HELD lock must refuse the replace.
    #[test]
    fn a_slot_with_a_held_cargo_lock_is_not_replaced() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = warm_shared(tmp.path());
        let pool = pool(&tmp.path().join("pool"));
        let slot = pool.slot_path(0);
        std::fs::create_dir_all(slot.join("debug")).expect("a live build dir");
        std::fs::write(slot.join("debug/in-progress.rlib"), b"live").expect("an artifact");
        let lock_path = slot.join("debug/.cargo-lock");
        let cargo = std::fs::File::create(&lock_path).expect("cargo's lock file");
        cargo
            .lock()
            .expect("hold the lock the way a running cargo does");

        let err = pool
            .seed(0, Some(&shared))
            .expect_err("a slot holding a live build must not be replaced");

        match err {
            SlotPoolError::ActiveBuild { path, lock } => {
                assert_eq!(path, slot);
                assert_eq!(lock, lock_path, "the refusal names the held lock");
            }
            other => panic!("expected ActiveBuild, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(slot.join("debug/in-progress.rlib")).expect("the build's artifact"),
            b"live",
            "the live build's tree is left as it was"
        );
        assert!(
            !slot.join("sentinel").exists(),
            "nothing was cloned over it"
        );
        assert!(
            !slot.join(SEED_MARKER).exists(),
            "a refusal writes no marker"
        );
        assert!(matches!(
            pool.reserve_path(0).expect("a writable root"),
            SlotReservation::Seeding(_)
        ));
        drop(cargo);
    }

    /// #8794: cargo leaves `.cargo-lock` behind after every build, so a lock
    /// FILE nobody holds must not block the seed — only a held lock does.
    #[test]
    fn a_stale_unheld_cargo_lock_does_not_block_seeding() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = warm_shared(tmp.path());
        let pool = pool(&tmp.path().join("pool"));
        let slot = pool.slot_path(0);
        std::fs::create_dir_all(slot.join("debug")).expect("a finished build dir");
        std::fs::write(slot.join("debug/.cargo-lock"), b"").expect("a stale lock file");

        let (path, seed) = pool
            .seed(0, Some(&shared))
            .expect("a stale lock file is no live build");

        assert!(marker_reads_seeded(&path), "the slot is seeded");
        if cfg!(target_os = "macos") {
            assert_eq!(seed, SeedKind::ClonedFromShared);
            assert!(path.join("sentinel").is_file(), "the clone landed");
        }
    }

    /// Restores a directory's mode on drop, so the temp root can be removed.
    struct Writable(PathBuf);
    impl Drop for Writable {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            drop(std::fs::set_permissions(
                &self.0,
                std::fs::Permissions::from_mode(0o755),
            ));
        }
    }

    /// #8794: a replace that fails partway (slot-6: `Directory not empty`)
    /// leaves a tree in an unknown state. It must write NO marker — the old
    /// code stamped `ColdDirectory("<error>")`, which read as seeded — and the
    /// failure is recorded beside the slot instead.
    #[test]
    fn a_failed_replace_leaves_no_seeded_marker() {
        if !cfg!(target_os = "macos") {
            // No clone runs off macOS, so there is no replace to fail.
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = warm_shared(tmp.path());
        let pool = pool(&tmp.path().join("pool"));
        let slot = pool.slot_path(0);
        let pinned = slot.join("debug/pinned");
        std::fs::create_dir_all(&pinned).expect("an unseeded slot tree");
        std::fs::write(pinned.join("artifact.rlib"), b"x").expect("an undeletable file");
        std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o555))
            .expect("make the replace fail");
        let _restore = Writable(pinned.clone());

        let err = pool
            .seed(0, Some(&shared))
            .expect_err("a failed replace is a failed seed");

        assert!(
            matches!(&err, SlotPoolError::SeedFailed { detail, .. } if detail.contains("could not replace")),
            "{err:?}"
        );
        assert!(
            !slot.join(SEED_MARKER).exists(),
            "a failed seed must leave no marker"
        );
        let recorded = std::fs::read_to_string(seed_failure_path(&slot)).expect("the failure");
        assert!(recorded.contains("could not replace"), "{recorded}");
        assert!(matches!(
            pool.reserve_path(0).expect("a writable root"),
            SlotReservation::Seeding(_)
        ));
    }

    /// #8794 migration: slot-6's marker holds the text of a failed replace. It
    /// must read as unseeded, and the next seed must replace it with a real one.
    #[test]
    fn a_failure_text_marker_is_treated_as_unseeded() {
        let tmp = tempfile::tempdir().expect("temp root");
        let shared = warm_shared(tmp.path());
        let pool = pool(&tmp.path().join("pool"));
        let slot = pool.slot_path(0);
        std::fs::create_dir_all(&slot).expect("a slot");
        std::fs::write(
            slot.join(SEED_MARKER),
            "#8261 builder slot pool\nseed: ColdDirectory(\"could not replace the unseeded \
             /pool/slot-6: Directory not empty (os error 66)\")\n",
        )
        .expect("a pre-#8794 failure marker");

        assert!(
            matches!(
                pool.reserve_path(0).expect("a writable root"),
                SlotReservation::Seeding(_)
            ),
            "a failure-text marker must not make the slot Ready"
        );
        let (path, seed) = pool.seed(0, Some(&shared)).expect("the re-seed");

        assert_ne!(seed, SeedKind::AlreadySeeded, "the slot was re-seeded");
        assert!(marker_reads_seeded(&path), "the new marker reads as seeded");
        assert!(
            !seed_failure_path(&path).exists(),
            "a successful seed clears the failure record"
        );
    }

    /// #8794 migration: a pre-#8794 marker reads as seeded exactly when the
    /// current code would have written it.
    #[test]
    fn legacy_markers_read_as_seeded_only_when_the_seed_succeeded() {
        let head = "#8261 builder slot pool\n";
        let cases = [
            ("seed: ClonedFromShared\n", true),
            (
                "seed: ColdDirectory(\"no warm shared target directory to clone from\")\n",
                true,
            ),
            (
                "seed: ColdDirectory(\"copy-on-write clone is macOS/APFS only; this is linux\")\n",
                true,
            ),
            (
                "seed: ColdDirectory(\"could not replace the unseeded /p/slot-6: \
                 Directory not empty (os error 66)\")\n",
                false,
            ),
            (
                "seed: ColdDirectory(\"cp -c exited exit status: 1: cp: no such file\")\n",
                false,
            ),
            ("", false),
        ];
        for (seed_line, seeded) in cases {
            let body = format!("{head}{seed_line}");
            assert_eq!(body_reads_seeded(&body), seeded, "{body:?}");
        }
        assert!(
            !body_reads_seeded(""),
            "an empty marker is a half-written one"
        );
    }

    #[test]
    fn two_slots_are_two_directories() {
        let tmp = tempfile::tempdir().expect("temp root");
        let pool = pool(&tmp.path().join("pool"));
        let (a, _) = pool.seed(0, None).expect("slot 0");
        let (b, _) = pool.seed(1, None).expect("slot 1");
        assert_ne!(a, b, "two concurrent builders never share a directory");
        assert!(a.is_dir() && b.is_dir());
    }
}
