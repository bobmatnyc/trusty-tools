//! Record-then-replay parity for the Architect script ports (#12026).
//!
//! Why: every slice of epic #12025 switches a Python script to Rust only after
//! the Rust binary reproduces the script's recorded behaviour. Hand-written
//! parity tests per module do not scale to 193 scripts, and this repository is
//! public, so a recording must never carry a local path, a session name or a
//! secret.
//! What: [`record`] runs a script on a sandbox copy of its inputs with a
//! cleared environment and captures argv, stdin, the allowlisted env, the
//! input files, the files it opened, stdout, stderr, the exit code and the
//! files it wrote. [`Scrubber`] replaces local paths and session names with
//! tokens, and [`check`] refuses a recording that still holds a home path, a
//! session name or a secret-shaped value. [`replay`] runs a target binary on
//! a fresh sandbox built from the recording and reports every differing
//! channel. [`save`] is the only fixture writer and checks before it writes.
//! A recording may declare [`Fakes`]: stub executables first on `PATH`
//! whose answers and calls the recording holds (#12028), and a frozen
//! [`Recording::clock`]: the recorder runs a Python script under a wrapper
//! that pins its wall clock, and replay hands the same instant to the target
//! in [`CLOCK_ENV`] (#12028).
//! Test: `tests/replay_tool.rs`, `tests/replay_level_lint.rs`.

mod diff;
mod fakes;
mod record;
mod sandbox;
mod scrub;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use diff::{Channel, Mismatch, Target, replay};
pub use fakes::{FakeAnswer, Fakes, GUARDED};
pub use record::{EnvRule, RecordSpec, record};
pub use scrub::{Refusal, RefusalKind, SKILL_NAMES, Scrubber, check};

/// The recording format version this build reads and writes.
pub const VERSION: u32 = 1;
/// The version of a recording that declares [`Fakes`] (#12028). A version-1
/// reader ignores the `fakes` field and would replay without the stubs, so a
/// recording with fakes carries this version and an old reader refuses it.
/// A recording without fakes stays version 1, byte for byte.
pub const FAKES_VERSION: u32 = 2;
/// The version of a recording with a [`Recording::clock`], with or without
/// fakes (#12028). A version-2 reader would replay it on the live clock, so
/// it refuses this version.
pub const CLOCK_VERSION: u32 = 3;
/// The variable that carries a recording's frozen instant (epoch seconds) to
/// the program, on record and on replay; never recorded.
///
/// Why: the target of a replay is a separate process, and the instant has to
/// reach it without a setting the shipped binaries obey (#12028).
/// What: the Python wrapper pops it and freezes the clock; a Rust replay
/// entry point (an example or test binary, never a shipped `[[bin]]`) reads
/// it and passes a fixed [`crate::clock::Clock`] to the ported code.
/// Test: `a_clocked_script_replays_clean_against_a_stand_in_that_reads_the_seam`,
/// `a_stand_in_that_ignores_the_clock_seam_reports_a_diff`.
pub const CLOCK_ENV: &str = "TRUSTY_REPLAY_CLOCK";
/// The token that stands for the sandbox root in every recorded string.
pub const ROOT_TOKEN: &str = "{ROOT}";
/// The fixed `PATH` both sides run under; the parent's `PATH` is never recorded.
pub const SANDBOX_PATH: &str = "/usr/bin:/bin";

/// One recorded script run. Every path is relative to [`ROOT_TOKEN`].
///
/// Invariant: a recording read through [`load`] or written through [`save`]
/// has passed [`check`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// [`VERSION`] at the time of recording.
    pub version: u32,
    /// Arguments after the program name.
    pub argv: Vec<String>,
    /// Bytes written to the program's stdin.
    pub stdin: String,
    /// The allowlisted environment, exactly as the program saw it.
    pub env: BTreeMap<String, String>,
    /// The input files laid into the sandbox before the run, by relative path.
    pub files: BTreeMap<String, String>,
    /// Sandbox files the program opened for reading, sorted; `None` when the
    /// program ran without the open probe (not a Python script).
    pub reads: Option<Vec<String>>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// The exit code.
    pub exit: i32,
    /// Files the run created or changed (`Some(content)`) or deleted (`None`).
    pub writes: BTreeMap<String, Option<String>>,
    /// Fake executables and the calls the run made to them; absent from the
    /// JSON when the recording declares none (#12028).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fakes: Option<Fakes>,
    /// The frozen instant, epoch seconds, both sides ran at; absent from the
    /// JSON when the run used the live clock (#12028).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<i64>,
}

impl Recording {
    /// The version this recording must carry: [`CLOCK_VERSION`] with a
    /// clock, [`FAKES_VERSION`] with fakes, else [`VERSION`].
    pub fn expected_version(&self) -> u32 {
        if self.clock.is_some() {
            CLOCK_VERSION // See #12028
        } else if self.fakes.is_some() {
            FAKES_VERSION
        } else {
            VERSION
        }
    }
}

/// A record, replay or fixture step that could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// A filesystem step failed; `what` names the step, never file content.
    #[error("{what}: {source}")]
    Io {
        /// The step, e.g. `copy seed`.
        what: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The program could not be started.
    #[error("could not start the program: {0}")]
    Spawn(#[source] std::io::Error),
    /// The program outlived the deadline and was killed.
    #[error("the program timed out and was killed")]
    Timeout,
    /// A signal ended the program at record time; there is no exit code to keep.
    #[error("the program was ended by a signal")]
    Signal,
    /// A recorded channel is not UTF-8.
    #[error("{0} is not UTF-8; only text channels are recorded")]
    NotUtf8(String),
    /// The script opened a file outside the sandbox (live state).
    #[error(
        "the script opened {count} file(s) outside the sandbox, first {first}; seed it instead"
    )]
    External {
        /// How many outside paths were opened.
        count: usize,
        /// The first one, with the home directory shown as `~`.
        first: String,
    },
    /// The run called a [`GUARDED`] executable the recording does not declare
    /// as a fake (#12028).
    #[error("the program called {0}, which is not declared as a fake; declare one")]
    Undeclared(String),
    /// The open probe's log is missing or lacks its first line, so the files
    /// the script opened are unknown (#12026).
    #[error("the open probe log is missing or damaged; the script's reads are unknown")]
    ProbeLog,
    /// A seed or the sandbox holds a symlink at this relative path (#12026).
    #[error("{0} is a symlink; only regular files and directories are recorded")]
    Symlink(String),
    /// No `python3` on the parent `PATH` for a `.py` program.
    #[error("no python3 on PATH")]
    NoPython,
    /// A seed or env argument is malformed.
    #[error("bad argument: {0}")]
    BadArg(String),
    /// The recording still holds a value that must not reach a fixture.
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    /// A fixture is not a recording this build reads.
    #[error("{path}: {reason}")]
    Format {
        /// The fixture path.
        path: PathBuf,
        /// Why it was rejected.
        reason: String,
    },
}

/// Writes `rec` to `path` as pretty JSON, after [`check`] passes.
///
/// Why: the only way a recording reaches a fixture, so an unscrubbed one is
/// refused before a byte is written.
/// What: runs [`check`] with `sessions`, then writes sorted-key JSON plus a
/// trailing newline; the same recording always gives the same bytes.
/// Test: `save_refuses_an_unscrubbed_recording_and_writes_nothing`.
pub fn save(path: &Path, rec: &Recording, sessions: &[String]) -> Result<(), ReplayError> {
    check(rec, sessions)?;
    let mut text = serde_json::to_string_pretty(rec).map_err(|e| ReplayError::Format {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    text.push('\n');
    std::fs::write(path, text).map_err(|source| ReplayError::Io {
        what: format!("write {}", path.display()),
        source,
    })
}

/// Reads a fixture, refusing a wrong version or one that fails [`check`].
/// The version must be [`Recording::expected_version`].
pub fn load(path: &Path) -> Result<Recording, ReplayError> {
    let format = |reason: String| ReplayError::Format {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|source| ReplayError::Io {
        what: format!("read {}", path.display()),
        source,
    })?;
    let rec: Recording = serde_json::from_str(&text).map_err(|e| format(e.to_string()))?;
    // See #12028: the version follows whether the recording declares fakes.
    if rec.version != rec.expected_version() {
        return Err(format(format!(
            "version {} (this build reads {VERSION}, {FAKES_VERSION} with fakes, \
             {CLOCK_VERSION} with a clock)",
            rec.version
        )));
    }
    check(&rec, &[])?;
    Ok(rec)
}
