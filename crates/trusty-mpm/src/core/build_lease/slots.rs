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
//! server) cannot pin the slot after the build ends. A `cargo run` program is
//! not such a child: cargo replaces itself with the program, so the program is
//! the build the holder waits on and keeps the slot until it exits, as does a
//! `cargo watch` watcher (unchanged; tracked in #8692). The other side of
//! close-on-exec: when the `tm build-lease` holder is SIGKILLed, its build
//! keeps running while the flock reads free. The record the dead holder left
//! in the slot file keeps the slot taken while the build it names is alive
//! ([`SlotState::Orphaned`], see `orphan`); a record that cannot be read or
//! checked makes the slot [`SlotState::Broken`] (#8736). The lease also checks
//! cargo's own `.cargo-lock` in the slot's target directory before it uses the
//! directory (see `stale_guard::cargo_lock_held`).
//!
//! Slot and admission files are created mode `0600`: a record names the
//! holder's command and checkout, and only the store's owner reads it.
//! Test: the `#[cfg(test)]` suite below uses real files and real `flock`s.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::orphan::{Leftover, read_leftover};

/// The directory name under `~/.trusty-mpm`.
pub const SLOT_DIR_NAME: &str = "build-slots";

/// What a live holder wrote into its slot file.
///
/// Why: a refusal has to name who holds the machine, and a census has to know
/// which compiler processes belong to a lease. Both read this.
/// What: the `tm build-lease` pid, the build's own pid once spawned, the
/// command summary ([`summarize_command`] — never the full argv), the
/// checkout, the start time and the target directory it was given.
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
    /// The command's summary: program, subcommand and `-p` packages.
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
    /// What: the command is summarized again here, so a record an older
    /// binary wrote with the full argv never prints its arguments (#8261
    /// round 3: a `--token` value must not reach another session's stderr).
    /// Test: `a_held_slot_is_reported_with_its_record`,
    /// `a_render_never_shows_an_argument_value`.
    #[must_use]
    pub fn render(&self) -> String {
        let age = chrono::DateTime::parse_from_rfc3339(&self.started_at)
            .map(|t| {
                let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds();
                format!("{}m", secs.max(0) / 60)
            })
            .unwrap_or_else(|_| "?m".to_string());
        let argv = shlex::split(&self.command).unwrap_or_else(|| {
            self.command
                .split_whitespace()
                .map(str::to_string)
                .collect()
        });
        format!(
            "slot {}: {} (pid {}, {age}, {})",
            self.slot,
            summarize_command(&argv),
            self.pid,
            self.cwd
        )
    }
}

/// The part of a build command that is safe to show another session.
///
/// Why: holder records are read by every waiter and logged by the daemon, and
/// a command line can carry a credential (`cargo publish --token …`, #8261
/// round 3).
/// What: the program's basename, the word after it (past `+toolchain`) when
/// that word is a plain lowercase name, and every `-p`/`--package` value before
/// `--`. Nothing else from the argv is kept.
/// Test: `a_render_never_shows_an_argument_value`.
#[must_use]
pub fn summarize_command(argv: &[String]) -> String {
    let Some((program, rest)) = argv.split_first() else {
        return String::new();
    };
    let mut out = vec![program.rsplit('/').next().unwrap_or(program).to_string()];
    let mut rest = rest.iter().skip_while(|w| w.starts_with('+')).peekable();
    if let Some(word) = rest.peek()
        && is_plain_name(word)
    {
        out.push((*word).clone());
        rest.next();
    }
    let rest: Vec<&String> = rest.take_while(|w| *w != "--").collect();
    for (i, word) in rest.iter().enumerate() {
        let package = match word.as_str() {
            "-p" | "--package" => rest.get(i + 1).map(|v| v.as_str()),
            w => w
                .strip_prefix("--package=")
                .or_else(|| w.strip_prefix("-p").filter(|v| !v.is_empty())),
        };
        if let Some(package) = package.filter(|p| is_plain_name(p)) {
            out.push(format!("-p {package}"));
        }
    }
    out.join(" ")
}

/// A lowercase name: `test`, `trusty-mpm`, `llvm-cov`, `30`.
fn is_plain_name(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && !word.starts_with(['-', '.'])
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
    /// The slot file could not be opened or locked, or a free slot's leftover
    /// record could not be read or checked (#8736); the error, rendered.
    /// Not counted as held: a broken file must not read as a build forever.
    /// Never taken either, and `tm doctor` FAILs on it.
    Broken(String),
    /// Nobody holds the flock, but the record a SIGKILLed holder left names a
    /// build still running (#8261). Counted as held, never taken.
    Orphaned(HolderRecord),
}

/// The build-slot directory.
///
/// Test: the module suite.
#[derive(Debug, Clone)]
pub struct SlotDir {
    root: PathBuf,
    /// Why the canonical store was not used, when this is the fallback.
    fallback: Option<String>,
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
        Ok(Self {
            root,
            fallback: None,
        })
    }

    /// This directory, marked as the fallback store because of `why`.
    #[must_use]
    pub(super) fn into_fallback(mut self, why: String) -> Self {
        self.fallback = Some(why);
        self
    }

    /// Why the canonical `~/.trusty-mpm/build-slots` is not in use, when this
    /// is the per-uid fallback store. `tm doctor` warns on it.
    ///
    /// Test: `a_broken_home_falls_back_to_the_fixed_per_uid_store`.
    #[must_use]
    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback.as_deref()
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
        let file = open_private(&path)?;
        if try_flock(&file)? {
            // A file an older binary created 0644 is narrowed on first use.
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            Ok(Some(SlotGuard {
                file,
                slot,
                path,
                keep_record: false,
            }))
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
    /// Test: `doctor_fails_on_a_broken_slot_file`.
    pub fn check_admission_lock(&self) -> Result<(), String> {
        let path = self.root.join("admission.lock");
        open_private(&path)
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
                SlotState::Orphaned(record) => Some(record),
                SlotState::Held(record) => Some(record.unwrap_or_else(|| HolderRecord {
                    slot,
                    pid: 0,
                    child_pid: None,
                    command: "unknown".to_string(),
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
            Ok(Some(guard)) => match guard.leftover() {
                Leftover::Clear => {
                    drop(guard);
                    SlotState::Free
                }
                Leftover::Running(record) => {
                    guard.release_keeping_record();
                    SlotState::Orphaned(record)
                }
                // #8736: an unreadable leftover never reads as Free.
                Leftover::Unknown(err) => {
                    guard.release_keeping_record();
                    SlotState::Broken(err)
                }
            },
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
    pub fn lock_admission(&self, deadline: Instant) -> std::io::Result<Option<AdmissionGuard>> {
        let file = open_private(&self.root.join("admission.lock"))?;
        loop {
            if try_flock(&file)? {
                return Ok(Some(AdmissionGuard { file }));
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
    /// Set by [`SlotGuard::release_keeping_record`]: `Drop` leaves the record.
    keep_record: bool,
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

    /// What the record a dead holder left in this slot means.
    ///
    /// Why: see `orphan` — a SIGKILLed holder's build may still run (#8261),
    /// and a record that cannot be checked must not free the slot (#8736).
    /// Test: `an_orphaned_build_keeps_its_slot`,
    /// `a_corrupt_record_in_a_free_slot_is_broken`.
    #[must_use]
    pub fn leftover(&self) -> Leftover {
        read_leftover(&self.path)
    }

    /// Release the lock but leave the record in the file.
    ///
    /// Why: a probe of an orphaned or broken slot must not erase the record
    /// that keeps it from being taken.
    /// What: consumes the guard, so `Drop` unlocks it explicitly, then closes.
    /// Test: `a_released_slot_is_free_while_this_process_spawns`.
    pub fn release_keeping_record(mut self) {
        self.keep_record = true;
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        // Cleared BEFORE the lock is released, so a probe never reads the old
        // holder's record on a slot that is already free.
        if !self.keep_record {
            let _ = self.file.set_len(0);
        }
        unlock(&self.file);
    }
}

/// The admission lock, held until dropped.
///
/// Test: `the_admission_lock_serialises_two_takers`.
#[derive(Debug)]
pub struct AdmissionGuard {
    file: File,
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

/// `flock(LOCK_UN)` before the descriptor closes.
///
/// Why: an unlock releases the lock on the open file description itself, so
/// it cannot outlive this call. A close only releases it when the last copy of
/// the descriptor closes.
/// What: on error, logs it and does nothing else. The close that follows still
/// releases the lock once every copy is gone, so the slot never reads free
/// while it is held.
/// Test: `a_released_slot_is_free_while_this_process_spawns`.
fn unlock(file: &File) {
    // #8736: an in-flight child's fd copy would otherwise hold the lock until its exec.
    // SAFETY: `file` owns a valid open descriptor for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
        tracing::debug!("flock(LOCK_UN) failed: {}", std::io::Error::last_os_error());
    }
}

/// Open (creating, mode `0600`) a lock file for reading and writing.
fn open_private(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
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

/// This process's real uid.
pub(super) fn current_uid() -> u32 {
    // SAFETY: getuid(2) cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

/// Every test here runs under the `build_slot_fds` key, shared by each
/// `build_lease` test that spawns a process (#8736). A child spawned on
/// another test thread holds a copy of every open descriptor until its
/// `exec`. `unlock` (`LOCK_UN`) is what frees a released slot at once
/// regardless; the key is a second guard that keeps the group's own spawners
/// apart from these tests.
#[cfg(test)]
#[serial_test::serial(build_slot_fds)]
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

    /// #8736: while another thread spawns children, each of which holds a
    /// copy of every descriptor until its exec, a dropped guard's slot and a
    /// probe's own lock are still free at once.
    #[test]
    fn a_released_slot_is_free_while_this_process_spawns() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let (_tmp, slots) = dir();
        let stop = Arc::new(AtomicBool::new(false));
        let spawned = Arc::new(AtomicU32::new(0));
        let spawner = {
            let (stop, spawned) = (Arc::clone(&stop), Arc::clone(&spawned));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let child = std::process::Command::new("/usr/bin/true").spawn();
                    if let Ok(mut child) = child {
                        let _ = child.wait();
                        spawned.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        };
        let started = Instant::now();
        let mut failure = None;
        let mut cycles = 0;
        while cycles < 5_000 && started.elapsed() < Duration::from_secs(3) {
            cycles += 1;
            // Errors end the loop as failures, never panics, so `stop` is
            // always set and the spawner always exits.
            let mut held = match slots.try_acquire(0) {
                Ok(Some(held)) => held,
                Ok(None) => {
                    failure = Some(format!(
                        "cycle {cycles}: the previous probe's lock was still held"
                    ));
                    break;
                }
                Err(err) => {
                    failure = Some(format!("cycle {cycles}: acquire failed: {err}"));
                    break;
                }
            };
            if let Err(err) = held.write_record(&record(0)) {
                failure = Some(format!("cycle {cycles}: write failed: {err}"));
                break;
            }
            drop(held);
            let probed = slots.probe();
            if probed != vec![(0, SlotState::Free)] {
                failure = Some(format!("cycle {cycles}: {probed:?} right after the drop"));
                break;
            }
        }
        stop.store(true, Ordering::Relaxed);
        spawner.join().expect("spawner thread");
        assert_eq!(
            failure,
            None,
            "{} children spawned",
            spawned.load(Ordering::Relaxed)
        );
        assert!(
            spawned.load(Ordering::Relaxed) > 0,
            "the spawner must overlap the cycles"
        );
    }

    /// #8261: a holder that dies without clearing its record (the SIGKILL
    /// case) leaves the slot taken while the build it names runs.
    #[test]
    fn an_orphaned_build_keeps_its_slot() {
        let (_tmp, slots) = dir();
        let mut build = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a stand-in build");
        let mut dead_holder = record(0);
        dead_holder.command = "sleep 30".into();
        dead_holder.child_pid = Some(build.id());
        leave_record(&slots, &dead_holder);
        let probed = slots.probe();
        let holders = slots.holders();
        let _ = build.kill();
        let _ = build.wait();
        assert_eq!(probed, vec![(0, SlotState::Orphaned(dead_holder))]);
        assert_eq!(holders.len(), 1, "an orphaned build counts as held");
        assert_eq!(slots.probe(), vec![(0, SlotState::Free)], "once it exits");
        assert!(slots.holders().is_empty());
    }

    /// Leave `record` in slot 0 the way a SIGKILLed holder does.
    fn leave_record(slots: &SlotDir, record: &HolderRecord) {
        let mut held = slots.try_acquire(0).expect("io").expect("free");
        held.write_record(record).expect("write");
        held.release_keeping_record();
    }

    /// Probe slot 0 holding a record whose build is a live `sleep`, after
    /// `edit` adjusts the record.
    fn probe_live_sleep(slots: &SlotDir, edit: impl FnOnce(&mut HolderRecord)) -> SlotState {
        let mut build = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a stand-in build");
        let mut dead_holder = record(0);
        dead_holder.command = "sleep 30".into();
        dead_holder.child_pid = Some(build.id());
        edit(&mut dead_holder);
        leave_record(slots, &dead_holder);
        let probed = slots.probe();
        let _ = build.kill();
        let _ = build.wait();
        probed.into_iter().next().expect("slot 0").1
    }

    /// #8736 fail-open check: a corrupt non-empty record in a free slot is
    /// Broken — never Free — and stays in the file for the operator.
    #[test]
    fn a_corrupt_record_in_a_free_slot_is_broken() {
        let (_tmp, slots) = dir();
        drop(slots.try_acquire(0).expect("io").expect("free"));
        let path = slots.path().join("slot-0.lock");
        let corrupt = r#"{"slot":0,"pid":12"#;
        std::fs::write(&path, corrupt).expect("corrupt");
        let probed = slots.probe();
        assert!(
            matches!(&probed[..], [(0, SlotState::Broken(e))] if e.contains("corrupt")),
            "{probed:?}"
        );
        assert_eq!(slots.broken().len(), 1, "tm doctor and acquire see it");
        assert!(slots.holders().is_empty(), "not counted as a holder");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            corrupt,
            "a probe never erases it"
        );
    }

    /// #8736 fail-open check: a live build whose record's `started_at` does
    /// not parse cannot be checked, so its slot is Broken.
    #[test]
    fn an_unparseable_started_at_is_broken() {
        let (_tmp, slots) = dir();
        let state = probe_live_sleep(&slots, |r| r.started_at = "yesterday".into());
        assert!(
            matches!(&state, SlotState::Broken(e) if e.contains("started_at")),
            "{state:?}"
        );
    }

    /// #8736 fail-open check: a pid the check cannot even ask about is Broken.
    #[test]
    fn an_uncheckable_pid_is_broken() {
        let (_tmp, slots) = dir();
        let mut dead_holder = record(0);
        dead_holder.child_pid = Some(u32::MAX);
        leave_record(&slots, &dead_holder);
        let probed = slots.probe();
        assert!(
            matches!(&probed[..], [(0, SlotState::Broken(e))] if e.contains("could not check")),
            "{probed:?}"
        );
    }

    /// #8736 (critic CRITICAL): cargo execs an external subcommand, so a
    /// `cargo nextest run` build's pid runs as `cargo-nextest`. A process name
    /// that differs from the record's program must not free a live build.
    #[test]
    fn an_exec_replaced_cargo_subcommand_keeps_its_slot() {
        let (tmp, slots) = dir();
        let nextest = tmp.path().join("cargo-nextest");
        std::os::unix::fs::symlink(
            which_sleep().expect("a sleep binary on this host"),
            &nextest,
        )
        .expect("symlink");
        let mut build = std::process::Command::new(&nextest)
            .arg("30")
            .spawn()
            .expect("spawn a process named cargo-nextest");
        let mut dead_holder = record(0);
        dead_holder.command = "cargo nextest run".into();
        dead_holder.child_pid = Some(build.id());
        leave_record(&slots, &dead_holder);
        let probed = slots.probe();
        let _ = build.kill();
        let _ = build.wait();
        assert_eq!(probed, vec![(0, SlotState::Orphaned(dead_holder))]);
    }

    /// The `sleep` binary, for a symlink that gives a process another name.
    fn which_sleep() -> Option<PathBuf> {
        ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
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

    /// #8261 round 3: a waiter must never see a holder's argument values.
    #[test]
    fn a_render_never_shows_an_argument_value() {
        let argv: Vec<String> = [
            "/usr/bin/cargo",
            "+1.94",
            "publish",
            "--token",
            "s3cr3t",
            "-p",
            "trusty-mpm",
            "--package=tc",
            "--",
            "-p",
            "hidden",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            summarize_command(&argv),
            "cargo publish -p trusty-mpm -p tc"
        );
        let mut rec = record(0);
        rec.command = "sh -c 'curl -H \"Authorization: x\"' --token s3cr3t".into();
        let line = rec.render();
        assert!(!line.contains("s3cr3t"), "{line}");
        assert!(!line.contains("Authorization"), "{line}");
        assert!(line.starts_with("slot 0: sh (pid "), "{line}");
    }

    #[test]
    fn slot_files_are_private_to_their_owner() {
        let (_tmp, slots) = dir();
        let held = slots.try_acquire(0).expect("io").expect("free");
        let mode = std::fs::metadata(held.path())
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        slots.check_admission_lock().expect("admission");
        let mode = std::fs::metadata(slots.path().join("admission.lock"))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
}
