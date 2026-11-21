//! The daemon: one instance, a liveness file, config reload, clean stop.
//!
//! Why: launchd restarts the scheduler, so a second copy must never run
//! (a double run fires every job twice), a hung scheduler must be visible
//! from outside, and a bad edit of `registry.json` must not stop it.
//! What: [`Instance::acquire`] takes the `flock` on `local/meta/sched.lock`
//! through the poller's [`PollLock`] (the kernel drops it on death). [`Reload`]
//! re-reads the config when its SHA-256 changes; a file that fails parsing or
//! validation keeps the previous config and raises one `meta_miss` per bad
//! content (a half-written file is re-read once after 200 ms first).
//! [`write_alive`] rewrites `local/meta/sched.alive` (`pid`, time, config
//! sha, job count, mode) atomically every tick. [`run_loop`] ticks, reloads,
//! touches liveness and sleeps until the stop flag is set, then waits for
//! running children up to [`STOP_WAIT`] (inside launchd's `ExitTimeOut`).
//! Test: `second_instance_is_refused_while_the_lock_is_held`,
//! `reload_keeps_the_previous_config_on_a_bad_file`, `liveness_file_is_fresh_after_a_tick`,
//! `run_loop_stops_on_the_flag_and_waits_for_children`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::config::Registry;
use super::cutover::{Mode, read_mode};
use super::engine::Engine;
use super::fresh::meta_alert;
use super::state::write_atomic;
use super::{Layout, SchedError};
use crate::classify::alerts::iso;
use crate::clock::Clock;
use crate::error::RunError;
use crate::poller::PollLock;
use crate::sha256::sha256_hex;

/// Seconds between ticks.
pub const TICK: Duration = Duration::from_secs(5);
/// How long a stop waits for running children.
pub const STOP_WAIT: Duration = Duration::from_secs(50);
/// A liveness file older than this is stale (`status` fails).
pub const STALE_AFTER_S: i64 = 120;

/// The held single-instance lock.
#[derive(Debug)]
pub struct Instance {
    _lock: PollLock,
}

impl Instance {
    /// Takes the lock without waiting.
    ///
    /// # Errors
    /// [`SchedError::Locked`] when another scheduler holds it.
    pub fn acquire(layout: &Layout) -> Result<Self, SchedError> {
        let path = layout.lock();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| SchedError::io(format!("create {}", dir.display()), e))?;
        }
        match PollLock::acquire(&path) {
            Ok(lock) => Ok(Self { _lock: lock }),
            Err(RunError::AlreadyRunning { path }) => Err(SchedError::Locked(path)),
            Err(e) => Err(SchedError::Cutover(e.to_string())),
        }
    }
}

/// The liveness document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alive {
    /// The scheduler's pid.
    pub pid: u32,
    /// UTC ISO time of the write.
    pub ts: String,
    /// The same, Unix seconds.
    pub unix: i64,
    /// SHA-256 of the loaded config.
    pub config_sha: String,
    /// Jobs in the config.
    pub jobs: usize,
    /// `shadow` or `live`.
    pub mode: String,
}

/// Rewrites the liveness file.
///
/// # Errors
/// [`SchedError::Io`] when it cannot be written.
pub fn write_alive(
    layout: &Layout,
    now: i64,
    sha: &str,
    jobs: usize,
    mode: Mode,
) -> Result<(), SchedError> {
    let doc = Alive {
        pid: std::process::id(),
        ts: iso(now as f64),
        unix: now,
        config_sha: sha.to_owned(),
        jobs,
        mode: if mode == Mode::Live { "live" } else { "shadow" }.to_owned(),
    };
    let text = serde_json::to_string(&doc).map_err(|e| SchedError::Cutover(e.to_string()))?;
    write_atomic(&layout.alive(), text.as_bytes())
}

/// Reads the liveness file; `None` when it is absent or unreadable.
pub fn read_alive(layout: &Layout) -> Option<Alive> {
    serde_json::from_str(&std::fs::read_to_string(layout.alive()).ok()?).ok()
}

/// Loads the config for the first time: a hard error on empty, unreadable or
/// invalid. Returns the registry and the SHA-256 of its text.
///
/// # Errors
/// As [`Registry::parse`], plus [`SchedError::Io`] on read.
pub fn load_initial(path: &Path) -> Result<(Registry, String), SchedError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SchedError::io(format!("read {}", path.display()), e))?;
    let reg = Registry::parse(&text, path)?;
    Ok((reg, sha256_hex(text.as_bytes())))
}

/// Config reload on change.
#[derive(Debug)]
pub struct Reload {
    path: std::path::PathBuf,
    /// SHA-256 of the config the engine runs.
    pub sha: String,
    bad: Option<String>,
}

impl Reload {
    /// Watches `path`, whose current content hashes to `sha`.
    pub fn new(path: &Path, sha: String) -> Self {
        Self {
            path: path.to_owned(),
            sha,
            bad: None,
        }
    }

    /// Reloads when the file changed; a bad file keeps the previous config.
    /// Returns true when the engine got a new config.
    pub fn poll(&mut self, engine: &mut Engine<'_>, clock: &dyn Clock) -> bool {
        let read = |path: &Path| std::fs::read_to_string(path);
        let Ok(mut text) = read(&self.path) else {
            return self.reject(engine, "registry.json is unreadable", "unreadable");
        };
        let mut sha = sha256_hex(text.as_bytes());
        if sha == self.sha {
            // Back to the running config: a later bad edit alerts again.
            self.bad = None;
            return false;
        }
        if self.bad.as_deref() == Some(&sha) {
            return false;
        }
        let mut parsed = Registry::parse(&text, &self.path);
        if parsed.is_err() {
            // A writer may be mid-write: read once more before refusing.
            clock.sleep(Duration::from_millis(200));
            if let Ok(again) = read(&self.path) {
                text = again;
                sha = sha256_hex(text.as_bytes());
                parsed = Registry::parse(&text, &self.path);
            }
        }
        match parsed {
            Ok(reg) => {
                tracing::info!(jobs = reg.jobs.len(), "config reloaded");
                engine.set_registry(reg);
                self.sha = sha;
                self.bad = None;
                true
            }
            Err(e) => {
                tracing::error!(error = %e, "config rejected; keeping the previous one");
                self.reject(
                    engine,
                    "registry.json failed validation; previous config kept",
                    &sha,
                )
            }
        }
    }

    fn reject(&mut self, engine: &Engine<'_>, detail: &str, sha: &str) -> bool {
        if self.bad.as_deref() != Some(sha) {
            self.bad = Some(sha.to_owned());
            engine.send_alert(&meta_alert(engine.now(), "config", detail));
        }
        false
    }
}

/// Ticks until `stop` is set (or `max_ticks` pass); see the module doc.
///
/// # Errors
/// Only a failed state save ends the loop; a failing job never does.
pub fn run_loop(
    engine: &mut Engine<'_>,
    reload: &mut Reload,
    clock: &dyn Clock,
    stop: &AtomicBool,
    max_ticks: Option<u64>,
) -> Result<(), SchedError> {
    run_loop_with(engine, reload, clock, stop, max_ticks, STOP_WAIT)
}

/// [`run_loop`] with the stop wait given (tests use a short one).
///
/// # Errors
/// As [`run_loop`].
pub fn run_loop_with(
    engine: &mut Engine<'_>,
    reload: &mut Reload,
    clock: &dyn Clock,
    stop: &AtomicBool,
    max_ticks: Option<u64>,
    stop_wait: Duration,
) -> Result<(), SchedError> {
    let mut ticks = 0;
    while !stop.load(Ordering::SeqCst) && max_ticks.is_none_or(|m| ticks < m) {
        reload.poll(engine, clock);
        engine.tick()?;
        let mode = read_mode(&engine.layout).unwrap_or(Mode::Shadow);
        let now = engine.now();
        if let Err(e) = write_alive(
            &engine.layout,
            now,
            &reload.sha,
            engine.registry().jobs.len(),
            mode,
        ) {
            tracing::error!(error = %e, "liveness file not written");
        }
        ticks += 1;
        if max_ticks.is_none_or(|m| ticks < m) {
            clock.sleep(TICK);
        }
    }
    engine.shutdown(stop_wait);
    Ok(())
}
