//! The machine-wide build slots: one `flock(2)` per slot file (#8261).
//!
//! Why: a slot must be released when its build ends however it ends — exit,
//! panic, SIGKILL, a Bash tool timeout. A lease record in a daemon needs a TTL
//! to recover from a holder that never reports back, and the 45-minute TTL was
//! shorter than a 201-minute cold compile, so a live build lost its slot index
//! to the next builder. An advisory lock on an open file needs no TTL: the
//! kernel drops it when the last descriptor closes, and a process that dies
//! closes every descriptor it had.
//!
//! What: [`SlotDir`] is `~/.trusty-mpm/build-slots/`. Slot `K` is
//! `slot-K.lock`; holding `LOCK_EX` on it IS holding the slot, and its contents
//! are the holder's [`HolderRecord`] for `tm doctor` and refusal messages.
//! `admission.lock` serialises the count-then-acquire step across processes so
//! two waiters cannot both see one free slot. `slot-K.last` remembers which
//! checkout last used slot `K`, so a build prefers the slot whose target
//! directory already holds its own crates.
//!
//! The lock files are opened close-on-exec (Rust's default), so a build's own
//! children never inherit the lock: a daemon a build starts (an `sccache`
//! server, a `cargo run` service) cannot pin the slot after the build ends.
//! Test: the `#[cfg(test)]` suite below uses real files and real `flock`s.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The directory name under `~/.trusty-mpm`.
pub const SLOT_DIR_NAME: &str = "build-slots";

/// What a live holder wrote into its slot file.
///
/// Why: a refusal has to name who holds the machine, and a census has to know
/// which compiler processes belong to a lease. Both read this.
/// What: the `tm build-lease` pid, the build's own pid once spawned, the
/// command, the checkout, the start time and the target directory it was
/// given.
/// Test: `a_held_slot_is_reported_with_its_record`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HolderRecord {
    /// The slot index.
    pub slot: u32,
    /// The `tm build-lease` process holding the lock.
    pub pid: u32,
    /// The build it spawned, once spawned.
    pub child_pid: Option<u32>,
    /// The command, as one line.
    pub command: String,
    /// The working directory it ran in.
    pub cwd: String,
    /// RFC 3339 start time.
    pub started_at: String,
    /// The `CARGO_TARGET_DIR` the build was given, if any.
    pub target_dir: Option<String>,
}

impl HolderRecord {
    /// A record for `slot`, held by this process, started now.
    #[must_use]
    pub fn new(slot: u32, command: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            slot,
            pid: std::process::id(),
            child_pid: None,
            command: command.into(),
            cwd: cwd.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            target_dir: None,
        }
    }

    /// `slot 1: cargo test -p x (pid 4412, 12m, /repo)`.
    ///
    /// Test: `a_held_slot_is_reported_with_its_record`.
    #[must_use]
    pub fn render(&self) -> String {
        let age = chrono::DateTime::parse_from_rfc3339(&self.started_at)
            .map(|t| {
                let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds();
                format!("{}m", secs.max(0) / 60)
            })
            .unwrap_or_else(|_| "?m".to_string());
        format!(
            "slot {}: {} (pid {}, {age}, {})",
            self.slot, self.command, self.pid, self.cwd
        )
    }
}

/// One slot as a probe found it.
///
/// Test: `a_held_slot_is_reported_with_its_record`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SlotState {
    /// Nobody holds it.
    Free,
    /// Somebody holds it; their record, when it parsed.
    Held(Option<HolderRecord>),
    /// The slot file could not be opened or locked; the error, rendered.
    /// Not counted as held: a broken file must not read as a build forever.
    Broken(String),
}

/// The build-slot directory.
///
/// Test: the module suite.
#[derive(Debug, Clone)]
pub struct SlotDir {
    root: PathBuf,
}

/// Why no slot directory could be used.
///
/// Test: `an_uncreatable_slot_dir_is_an_error_naming_the_path`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SlotDirError {
    /// The directory could not be created.
    #[error("could not create build-slot directory {path}: {source}")]
    Create {
        /// The path tried.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

impl SlotDir {
    /// Open (creating) the slot directory at `root`.
    ///
    /// # Errors
    ///
    /// [`SlotDirError::Create`] naming the path.
    ///
    /// Test: `an_uncreatable_slot_dir_is_an_error_naming_the_path`.
    pub fn at(root: impl Into<PathBuf>) -> Result<Self, SlotDirError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| SlotDirError::Create {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    /// `<home>/.trusty-mpm/build-slots`, else a per-user temp fallback.
    ///
    /// Why: the fail-open table in [`crate::core::build_lease`] — a home
    /// directory whose `.trusty-mpm` cannot be written must not stop every
    /// build, and a temp directory keyed by uid still coordinates every build
    /// this user runs.
    /// What: the home path first; on failure, `$TMPDIR/trusty-mpm-build-slots-<uid>`.
    ///
    /// # Errors
    ///
    /// Both attempts' failures, the home one first.
    ///
    /// Test: `the_temp_fallback_is_used_when_home_is_unwritable`.
    pub fn resolve(home: &Path) -> Result<Self, (SlotDirError, SlotDirError)> {
        let primary = home.join(".trusty-mpm").join(SLOT_DIR_NAME);
        match Self::at(&primary) {
            Ok(dir) => Ok(dir),
            Err(first) => {
                let fallback = std::env::temp_dir()
                    .join(format!("trusty-mpm-{SLOT_DIR_NAME}-{}", current_uid()));
                Self::at(fallback).map_err(|second| (first, second))
            }
        }
    }

    /// The directory itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    fn lock_path(&self, slot: u32) -> PathBuf {
        self.root.join(format!("slot-{slot}.lock"))
    }

    fn last_path(&self, slot: u32) -> PathBuf {
        self.root.join(format!("slot-{slot}.last"))
    }

    /// Try to take slot `slot` without blocking.
    ///
    /// What: `Ok(Some(guard))` holds it; `Ok(None)` means another open file
    /// description holds it; `Err` is an open or `flock` failure other than
    /// "would block".
    ///
    /// # Errors
    ///
    /// The open or `flock` error.
    ///
    /// Test: `a_slot_is_exclusive_across_open_file_descriptions`.
    pub fn try_acquire(&self, slot: u32) -> std::io::Result<Option<SlotGuard>> {
        let path = self.lock_path(slot);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if try_flock(&file)? {
            Ok(Some(SlotGuard { file, slot, path }))
        } else {
            Ok(None)
        }
    }

    /// Every slot file present, probed, lowest index first.
    ///
    /// Why: holders are counted from the files, not from `0..ceiling`, so a
    /// holder admitted under a higher ceiling is still counted after the
    /// operator lowers it.
    /// What: a free slot is probed by taking and immediately dropping its lock.
    /// Test: `a_held_slot_is_reported_with_its_record`.
    #[must_use]
    pub fn probe(&self) -> Vec<(u32, SlotState)> {
        let mut out: Vec<(u32, SlotState)> = self
            .slot_indices()
            .into_iter()
            .map(|slot| (slot, self.probe_one(slot)))
            .collect();
        out.sort_by_key(|(slot, _)| *slot);
        out
    }

    /// Slot files that cannot be opened or locked, with the error.
    ///
    /// Why: a broken slot file is neither capacity nor a holder; `acquire`
    /// skips it and `tm doctor` FAILS on it (#8261 critic round 1).
    /// Test: `a_broken_lowest_slot_is_skipped`.
    #[must_use]
    pub fn broken(&self) -> Vec<(u32, String)> {
        self.probe()
            .into_iter()
            .filter_map(|(slot, state)| match state {
                SlotState::Broken(err) => Some((slot, err)),
                _ => None,
            })
            .collect()
    }

    /// Whether `admission.lock` can be opened, without taking it.
    ///
    /// # Errors
    ///
    /// The open error, rendered with the path.
    ///
    /// Test: `an_unopenable_admission_lock_is_bounded`.
    pub fn check_admission_lock(&self) -> Result<(), String> {
        let path = self.root.join("admission.lock");
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map(drop)
            .map_err(|err| format!("{}: {err}", path.display()))
    }

    /// The live holders, lowest slot first.
    ///
    /// Test: `a_held_slot_is_reported_with_its_record`.
    #[must_use]
    pub fn holders(&self) -> Vec<HolderRecord> {
        self.probe()
            .into_iter()
            .filter_map(|(slot, state)| match state {
                SlotState::Held(record) => Some(record.unwrap_or_else(|| HolderRecord {
                    slot,
                    pid: 0,
                    child_pid: None,
                    command: "unknown (record not yet written)".to_string(),
                    cwd: String::new(),
                    started_at: String::new(),
                    target_dir: None,
                })),
                SlotState::Free | SlotState::Broken(_) => None,
            })
            .collect()
    }

    fn probe_one(&self, slot: u32) -> SlotState {
        match self.try_acquire(slot) {
            Ok(Some(guard)) => {
                drop(guard);
                SlotState::Free
            }
            Ok(None) => SlotState::Held(read_record(&self.lock_path(slot))),
            // #8261 fail-open table: a broken slot file is neither capacity nor
            // a holder; `acquire` skips it and runs unleased if all are broken.
            Err(err) => SlotState::Broken(err.to_string()),
        }
    }

    fn slot_indices(&self) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_prefix("slot-")?
                    .strip_suffix(".lock")?
                    .parse::<u32>()
                    .ok()
            })
            .collect()
    }

    /// Slot indices `0..limit`, the one last used by `checkout` first.
    ///
    /// Why: path crates fingerprint by absolute path, so a slot whose target
    /// directory last built this checkout rebuilds far less.
    /// Test: `the_slot_last_used_by_this_checkout_is_preferred`.
    #[must_use]
    pub fn preference_order(&self, limit: u32, checkout: &str) -> Vec<u32> {
        let mut order: Vec<u32> = (0..limit).collect();
        if let Some(pos) = order.iter().position(|slot| {
            std::fs::read_to_string(self.last_path(*slot)).is_ok_and(|s| s.trim() == checkout)
        }) {
            let preferred = order.remove(pos);
            order.insert(0, preferred);
        }
        order
    }

    /// Remember that `checkout` used `slot`. Best effort.
    pub fn remember_checkout(&self, slot: u32, checkout: &str) {
        if let Err(err) = std::fs::write(self.last_path(slot), checkout) {
            tracing::debug!("could not record slot {slot} affinity: {err}");
        }
    }

    /// Take the admission lock, polling until `deadline`.
    ///
    /// Why: counting holders and taking a slot must be one step machine-wide,
    /// or two waiters both see the last free slot.
    /// What: `Ok(Some(guard))` once held, `Ok(None)` at the deadline.
    ///
    /// # Errors
    ///
    /// An open or `flock` error.
    ///
    /// Test: `the_admission_lock_serialises_two_takers`.
    pub fn lock_admission(&self, deadline: Instant) -> std::io::Result<Option<File>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("admission.lock"))?;
        loop {
            if try_flock(&file)? {
                return Ok(Some(file));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A held slot. Dropping it clears the record and releases the lock.
///
/// Test: `dropping_the_guard_frees_the_slot`.
#[derive(Debug)]
pub struct SlotGuard {
    file: File,
    slot: u32,
    path: PathBuf,
}

impl SlotGuard {
    /// The slot index held.
    #[must_use]
    pub fn slot(&self) -> u32 {
        self.slot
    }

    /// Overwrite the slot file with `record`.
    ///
    /// # Errors
    ///
    /// The write error; the lock is held regardless.
    ///
    /// Test: `a_held_slot_is_reported_with_its_record`.
    pub fn write_record(&mut self, record: &HolderRecord) -> std::io::Result<()> {
        let body = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        self.file.set_len(0)?;
        self.file.rewind()?;
        self.file.write_all(&body)?;
        self.file.flush()
    }

    /// The lock file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        // Cleared BEFORE the descriptor closes, so a probe never reads the old
        // holder's record on a slot that is already free.
        let _ = self.file.set_len(0);
    }
}

/// Read a slot file's record, if it holds one.
fn read_record(path: &Path) -> Option<HolderRecord> {
    let mut body = String::new();
    File::open(path).ok()?.read_to_string(&mut body).ok()?;
    serde_json::from_str(&body).ok()
}

/// `flock(LOCK_EX | LOCK_NB)`: `Ok(true)` held, `Ok(false)` would block.
fn try_flock(file: &File) -> std::io::Result<bool> {
    // SAFETY: `file` owns a valid open descriptor for the duration of the call.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// This process's real uid, for the temp fallback's name.
fn current_uid() -> u32 {
    // SAFETY: getuid(2) cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> (tempfile::TempDir, SlotDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let slots = SlotDir::at(tmp.path().join("slots")).expect("slot dir");
        (tmp, slots)
    }

    fn record(slot: u32) -> HolderRecord {
        HolderRecord {
            slot,
            pid: std::process::id(),
            child_pid: Some(1),
            command: "cargo test -p x".into(),
            cwd: "/repo".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            target_dir: None,
        }
    }

    #[test]
    fn a_slot_is_exclusive_across_open_file_descriptions() {
        let (_tmp, slots) = dir();
        let held = slots.try_acquire(0).expect("io").expect("free");
        assert!(
            slots.try_acquire(0).expect("io").is_none(),
            "a second open must not take it"
        );
        assert!(
            slots.try_acquire(1).expect("io").is_some(),
            "another slot is independent"
        );
        drop(held);
    }

    #[test]
    fn dropping_the_guard_frees_the_slot() {
        let (_tmp, slots) = dir();
        let mut held = slots.try_acquire(0).expect("io").expect("free");
        held.write_record(&record(0)).expect("write");
        drop(held);
        assert_eq!(slots.probe(), vec![(0, SlotState::Free)]);
        assert!(slots.holders().is_empty());
    }

    #[test]
    fn a_held_slot_is_reported_with_its_record() {
        let (_tmp, slots) = dir();
        let mut held = slots.try_acquire(2).expect("io").expect("free");
        held.write_record(&record(2)).expect("write");
        let holders = slots.holders();
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].slot, 2);
        assert_eq!(holders[0].pid, std::process::id());
        assert_eq!(holders[0].child_pid, Some(1));
        let line = holders[0].render();
        assert!(line.starts_with("slot 2: cargo test -p x (pid "), "{line}");
        assert!(line.ends_with(", 0m, /repo)"), "{line}");
        drop(held);
    }

    #[test]
    fn the_admission_lock_serialises_two_takers() {
        let (_tmp, slots) = dir();
        let first = slots
            .lock_admission(Instant::now() + Duration::from_secs(1))
            .expect("io")
            .expect("free");
        let second = slots
            .lock_admission(Instant::now() + Duration::from_millis(100))
            .expect("io");
        assert!(
            second.is_none(),
            "the second taker must time out while the first holds it"
        );
        drop(first);
        assert!(slots.lock_admission(Instant::now()).expect("io").is_some());
    }

    #[test]
    fn the_slot_last_used_by_this_checkout_is_preferred() {
        let (_tmp, slots) = dir();
        slots.remember_checkout(2, "/repo/a");
        assert_eq!(slots.preference_order(4, "/repo/a"), vec![2, 0, 1, 3]);
        assert_eq!(slots.preference_order(4, "/repo/b"), vec![0, 1, 2, 3]);
    }

    #[test]
    fn an_uncreatable_slot_dir_is_an_error_naming_the_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, "x").expect("write");
        let err = SlotDir::at(file.join("slots")).expect_err("a file cannot hold a directory");
        assert!(err.to_string().contains("not-a-dir"), "{err}");
    }

    #[test]
    fn the_temp_fallback_is_used_when_home_is_unwritable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // `.trusty-mpm` is a FILE, so `.trusty-mpm/build-slots` cannot exist.
        std::fs::write(tmp.path().join(".trusty-mpm"), "x").expect("write");
        let slots = SlotDir::resolve(tmp.path()).expect("the temp fallback works");
        assert!(
            slots.path().starts_with(std::env::temp_dir()),
            "{:?}",
            slots.path()
        );
    }
}
