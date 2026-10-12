//! The loop's state across cycles, and what it does with a state file it
//! cannot use.
//!
//! Why: the prototype's `load_state` turns an unreadable or corrupt
//! `poll-state.json` into `{}` and its next save overwrites the file, so the
//! keys Python owns and any owed wake are lost (#9803 critic carry-over,
//! #9171 A3b-2).
//! What: [`PollSession::load`] reads the state. A missing file is an empty
//! state. A file that cannot be read (I/O, not UTF-8) gives an empty
//! in-memory state and blocks every save while the error lasts. A corrupt
//! file is first copied to `poll-state.json.corrupt.<unix>` (`create_new`);
//! then the loop starts empty and may save. If that copy fails, saves stay
//! blocked. A blocked session re-reads the file before each cycle; once it
//! reads, the session adopts it and adds the wakes it owed meanwhile. A dry
//! run copies nothing.
//! Test: `load_state_io_skips_save`, `load_state_corrupt_keeps_copy`,
//! `corrupt_state_whose_copy_fails_is_never_overwritten`,
//! `blocked_state_recovers_and_keeps_owed_wake`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use super::config::Config;
use super::ctx::TranscriptCtx;
use super::state::{PollState, StoredCount, load_state};
use crate::error::StateError;

/// The state one poll loop carries from cycle to cycle.
#[derive(Debug, Default)]
pub struct PollSession {
    /// The state the next cycle starts from.
    pub state: PollState,
    /// Per-transcript token counts (the prototype's `_transcript_cache`).
    pub transcripts: TranscriptCtx,
    save_blocked: bool,
}

/// The outcome of reading the state file: a state the loop may run on and
/// save, or `None` when the file must not be saved over.
type Read = Option<Box<PollState>>;

/// Copies the corrupt file at `path` beside it, never over another file.
fn keep_copy(path: &Path, now: f64) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".corrupt.{}", now.max(0.0) as u64));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&name)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    Ok(name.to_string_lossy().into_owned())
}

/// Reads `config.state()`, logging into `log` what it found.
fn read_state(config: &Config, dry_run: bool, now: f64, log: &mut Vec<String>) -> Read {
    let path = config.state();
    match load_state(&path) {
        (state, None) => Some(Box::new(state)),
        (_, Some(e @ StateError::Corrupt { .. })) => {
            if dry_run {
                log.push(format!("state CORRUPT {e}; dry run: starting empty"));
                return Some(Box::default());
            }
            match keep_copy(&path, now) {
                Ok(copy) => {
                    log.push(format!(
                        "state CORRUPT {e}; kept a copy at {copy}; starting empty"
                    ));
                    Some(Box::default())
                }
                Err(c) => {
                    log.push(format!(
                        "state CORRUPT {e}; copy failed: {c}; running empty, not saving over it"
                    ));
                    None
                }
            }
        }
        (_, Some(e)) => {
            log.push(format!(
                "state ERROR {e}; running empty, not saving over it"
            ));
            None
        }
    }
}

impl PollSession {
    /// A session starting from `state`, free to save.
    pub fn new(state: PollState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }

    /// Loads `config.state()`; returns the session and the lines to log.
    pub fn load(config: &Config, dry_run: bool, now: f64) -> (Self, Vec<String>) {
        let mut log = Vec::new();
        let session = match read_state(config, dry_run, now, &mut log) {
            Some(state) => Self::new(*state),
            None => Self {
                save_blocked: true,
                ..Self::default()
            },
        };
        (session, log)
    }

    /// True while the state file must not be saved over.
    pub fn save_blocked(&self) -> bool {
        self.save_blocked
    }

    /// When blocked, re-reads the file; once it reads, adopts it with the
    /// wakes owed in memory added to its own.
    pub(crate) fn retry_load(
        &mut self,
        config: &Config,
        dry_run: bool,
        now: f64,
        log: &mut Vec<String>,
    ) {
        if !self.save_blocked {
            return;
        }
        let Some(mut loaded) = read_state(config, dry_run, now, log) else {
            return;
        };
        // #9171: a wake owed while the file was unreadable is still owed.
        let owed =
            |c: &Option<StoredCount>| c.as_ref().map_or(0, |c| c.or_unreadable("wake_pending", 1));
        let held = owed(&self.state.wake_pending);
        if held > 0 {
            let total = owed(&loaded.wake_pending).saturating_add(held);
            loaded.wake_pending = Some(StoredCount::Count(total));
        }
        log.push(format!(
            "state readable again; adopted it with {held} wake(s) owed meanwhile"
        ));
        self.state = *loaded;
        self.save_blocked = false;
    }
}
