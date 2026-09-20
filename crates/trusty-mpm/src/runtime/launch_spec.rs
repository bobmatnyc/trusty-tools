//! The parameter carrier a managed pane launch is driven by (#8233).
//!
//! Why: every managed launch used to be TYPED into the pane as one shell script
//! — `cd … && { export …; __tm_t0=…; . gh-env; rm -f gh-env; env -u … A=… B=…
//! <tm> internal-spawn-disclaimed <claude> --flags…; <exit dispatch> }`. Its
//! length grew with the cwd, `TMPDIR`, every env assignment and every flag, and
//! at 1054 bytes it crossed the tty's canonical-mode input limit
//! (`MAX_CANON`, 1024 on macOS — `sys/syslimits.h:89`, and `fpathconf(pty,
//! _PC_MAX_CANON)` agrees). tmux typed it before the pane shell's line editor
//! was up, the line discipline dropped everything past ~1020 bytes, and the
//! session died with a truncated command on screen. Shortening the line only
//! moves the cliff; the fix is to stop putting parameters in it at all.
//!
//! What: [`LaunchSpec`] is the whole launch — cwd, program, argv, env unsets and
//! env assignments — written to a mode-0600 JSON file inside a mode-0700
//! directory. The pane receives a FIXED-SHAPE invocation naming only that file,
//! and `tm internal-spawn-disclaimed --launch-spec <file>` resolves the rest
//! itself. Nothing on the typed line grows with the launch.
//!
//! Secrets (`CLAUDE_CODE_OAUTH_TOKEN`, `GH_TOKEN`) ride in `env_set`, so they
//! reach `claude`'s environment without ever being typed into the pane or
//! appearing in any process's argv. The file is created WITH mode 0600 (never
//! chmod'd after, so it is not world-readable for an instant) and
//! [`LaunchSpec::consume`] deletes it as it reads it.
//!
//! Test: `crates/trusty-mpm/src/runtime/launch_spec_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Directory under the tm config home that holds pending launch specs.
///
/// Why: a fixed, short, tm-owned root rather than `std::env::temp_dir()` — the
/// typed line carries this path, and `TMPDIR` on macOS is a ~50-character
/// per-user sandbox path that would put the launch's length back at the mercy
/// of the environment.
/// What: joined onto `~/.trusty-tools/trusty-mpm/`.
const SPEC_SUBDIR: &str = "launch-specs";

/// Owner-only mode for the spec directory (#8233 requirement: 0700).
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Owner-only mode for the spec file itself, applied AT CREATION.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// Extension of the "the pane ran the launch line" sentinel (#8233 review).
///
/// Why: `tmux send-keys` succeeding proves tmux accepted the keystrokes, not
/// that the pane's SHELL parsed and ran them. The whole defect this issue tracks
/// is a shell left at a continuation prompt with the line unexecuted, so the
/// launcher needs a signal that only exists once the shim actually started.
/// What: `<launch-id>.started`, written by [`LaunchSpec::mark_started`] beside
/// the spec it was read from and observed by
/// `daemon::managed_routes::launch_verify`.
///
/// #8233 review round 2 (HIGH): the sentinel is named for the LAUNCH, not for
/// the session. Keyed on the session, a marker written by an earlier launch of
/// the same session satisfied a later one that the shell never ran — the exact
/// false "it started" this handshake exists to rule out. A per-launch uuid
/// cannot be satisfied by anything but its own launch.
const STARTED_SUFFIX: &str = ".started";

/// Extension of the per-session pointer naming the CURRENT launch (#8233 review
/// round 2).
///
/// Why: the sentinel is keyed on the launch, but the daemon's post-send check
/// knows only the session record. This file is the one hop between them: the
/// launcher writes it immediately before typing, so the checker resolves
/// "this session's most recent launch" without the adapter having to thread the
/// id back through `RuntimeAdapter::spawn`'s `Result<(), RuntimeError>`.
/// What: `<session-uuid>.launch`, whose whole contents are the launch id.
const LAUNCH_POINTER_SUFFIX: &str = ".launch";

/// How long an unconsumed spec or sentinel may sit before it is reaped (#8233
/// review, HIGH).
///
/// Why: a spec carries `GH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` in cleartext and
/// is normally deleted within a second, as the shim reads it. But the pane can
/// be killed before the line runs, the daemon can restart, or the launch can be
/// abandoned — and nothing then deletes it. Unlike the `TMPDIR` file this
/// replaced, `~/.trusty-tools/…/launch-specs` is never swept by the OS, so an
/// orphan would keep a live token readable indefinitely.
/// What: ten minutes. A spec is consumed within seconds of being written, so
/// anything older is certainly an orphan; the margin covers a pane that is slow
/// to start its shell.
/// Test: `write_reaps_an_orphaned_spec`, `write_keeps_a_fresh_spec`.
const ORPHAN_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// What can go wrong carrying a launch's parameters to the pane.
///
/// Why: the spawn path must fail CLOSED — a spec that cannot be written or read
/// back must abort the launch and error the session record, never fall back to
/// typing an unbounded shell line. Structured variants let the caller say which
/// half failed in the message it puts in front of the operator.
/// What: one variant per step, each naming the path it was working on.
/// Test: `write_reports_an_unwritable_directory`, `consume_reports_a_missing_spec`,
/// `consume_reports_a_corrupt_spec`.
#[derive(Debug, Error)]
pub enum LaunchSpecError {
    /// The home directory could not be resolved, so there is no spec root.
    #[error("launch-spec directory unavailable: home directory could not be resolved")]
    NoRoot,

    /// The spec directory could not be created with owner-only permissions.
    #[error("launch-spec directory {path} could not be prepared: {source}")]
    Dir {
        /// The directory that could not be prepared.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The spec file could not be created or written.
    #[error("launch-spec {path} could not be written: {source}")]
    Write {
        /// The spec file that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The spec file could not be read back (missing, unreadable, deleted).
    #[error("launch-spec {path} could not be read: {source}")]
    Read {
        /// The spec file that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The spec file existed but did not hold a decodable [`LaunchSpec`].
    #[error("launch-spec {path} is corrupt: {source}")]
    Parse {
        /// The spec file that could not be decoded.
        path: PathBuf,
        /// The decode failure.
        source: serde_json::Error,
    },
}

/// Everything a managed `claude` launch needs, in structured form (#8233).
///
/// Why: the pane can only run a command; it must not have to CARRY the launch.
/// Moving cwd/argv/env off the typed line is what makes the line's length
/// independent of the launch — the whole point of the fix. It is also what lets
/// `claude` be `posix_spawn`ed directly by the disclaim shim (#2997) instead of
/// through an `env` process, so the resolved parameters are asserted against
/// this struct rather than re-parsed out of a shell string.
/// What: `session_id` is the managed session UUID (#2023 component B, exported
/// into the child so hooks and the in-place relaunch can identify the session);
/// `cwd` is the directory the child is rooted at (#2250); `program` is the
/// resolved absolute `claude` (#1298); `args` is its argv after the program;
/// `env_unset` names variables removed from the inherited pane environment
/// (`ANTHROPIC_API_KEY` plus `core::claude_env_scrub::INHERITED_SESSION_MARKERS`
/// and any inherited `gh` identity, #4467/#6668); `env_set` is every assignment,
/// applied after the unsets.
///
/// Field ORDER within `env_unset`/`env_set` is preserved on the wire so a test
/// can pin the composition against the shell line this replaced.
/// Test: `spec_round_trips_through_the_file`,
/// `spec_command_carries_cwd_program_argv_and_env`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// The managed session's UUID (`TM_MANAGED_SESSION_ID`).
    pub session_id: String,
    /// This LAUNCH's own id — the sentinel key (#8233 review round 2).
    ///
    /// Defaults on deserialize so a spec written by an older `tm` still decodes;
    /// an empty id simply keys a sentinel no checker is looking for, which reads
    /// as "not delivered" rather than as a false success.
    #[serde(default)]
    pub launch_id: String,
    /// Working directory for the spawned process.
    pub cwd: PathBuf,
    /// Absolute path to the program to launch (the resolved `claude`).
    pub program: String,
    /// The program's arguments, excluding the program itself.
    pub args: Vec<String>,
    /// Variables removed from the inherited environment, applied first.
    pub env_unset: Vec<String>,
    /// Variables assigned into the child environment, applied after the unsets.
    pub env_set: Vec<(String, String)>,
}

impl LaunchSpec {
    /// Resolve the tm-owned directory pending specs are written to.
    ///
    /// Why: one answer for both the writer (the daemon) and the reader (the
    /// shim), so neither can look in a directory the other does not use.
    /// What: `~/.trusty-tools/trusty-mpm/launch-specs`. `None` when the home
    /// directory cannot be resolved.
    /// Test: `spec_root_nests_under_the_tm_config_home`.
    pub fn root() -> Option<PathBuf> {
        trusty_common::crate_config::crate_config_dir(crate::core::trusty_tools_config::CRATE_NAME)
            .map(|dir| dir.join(SPEC_SUBDIR))
    }

    /// [`LaunchSpec::root`] under an explicitly named state home (#8233).
    ///
    /// Why: [`root`](Self::root) reads the process home, so a test driving the
    /// real managed launch left credential-bearing spec files in the operator's
    /// own `~/.trusty-tools/`. The daemon already holds the layout the launch
    /// belongs to — see [`crate::core::paths::FrameworkPaths::crate_config_root`].
    /// What: `<crate_config_root>/launch-specs`, the same subdirectory
    /// [`root`](Self::root) names.
    /// Test: `spec_root_at_nests_under_the_named_state_home`.
    pub fn root_at(crate_config_root: &Path) -> PathBuf {
        crate_config_root.join(SPEC_SUBDIR)
    }

    /// Write this spec to a fresh mode-0600 file under [`LaunchSpec::root`].
    ///
    /// Why: the launch's parameters have to reach the pane somehow, and a file
    /// the pane merely NAMES is the only carrier whose typed size is fixed.
    /// What: delegates to [`LaunchSpec::write_in`] with the production root.
    /// Test: `write_creates_an_owner_only_file_in_an_owner_only_dir`.
    pub fn write(&self) -> Result<PathBuf, LaunchSpecError> {
        let dir = Self::root().ok_or(LaunchSpecError::NoRoot)?;
        self.write_in(&dir)
    }

    /// [`LaunchSpec::write`] against an explicit directory (the hermetic seam).
    ///
    /// Why: tests must not write into the operator's real config home, and the
    /// permission assertions need a directory they control.
    /// What: creates `dir` with mode 0700 when absent, then writes
    /// `<dir>/<uuid>.json` through [`create_owner_only`] — created WITH mode
    /// 0600, never chmod'd afterwards, so the JSON (which carries
    /// `CLAUDE_CODE_OAUTH_TOKEN`/`GH_TOKEN`) is never group- or world-readable
    /// for even an instant.
    /// Test: `write_creates_an_owner_only_file_in_an_owner_only_dir`,
    /// `write_reports_an_unwritable_directory`.
    pub fn write_in(&self, dir: &Path) -> Result<PathBuf, LaunchSpecError> {
        ensure_owner_only_dir(dir).map_err(|source| LaunchSpecError::Dir {
            path: dir.to_path_buf(),
            source,
        })?;
        // #8233 review (HIGH): collect whatever earlier launches abandoned here
        // before adding one more. Every launch passes through this function, so
        // it is the one place a sweep is guaranteed to run.
        reap_orphans_in(dir, ORPHAN_TTL);
        let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
        let json = serde_json::to_vec(self).map_err(|e| LaunchSpecError::Write {
            path: path.clone(),
            source: std::io::Error::other(e),
        })?;
        create_owner_only(&path, &json).map_err(|source| LaunchSpecError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// Read a spec and DELETE it in the same step.
    ///
    /// Why: the spec is single-use and holds credentials. Removing it as part of
    /// reading it means a launch that proceeds leaves nothing on disk, and a
    /// stale file can never be replayed into a second `claude`.
    /// What: reads the bytes, removes the file, then decodes. A missing file, an
    /// unreadable one, or one that is not decodable JSON each returns the
    /// matching [`LaunchSpecError`], which is what makes the shim fail loudly
    /// instead of launching a partial `claude`.
    ///
    /// #8233 review (MEDIUM): the removal is best-effort — the parameters are
    /// already in hand, so failing the launch over it would trade a working
    /// session for a tidy directory — but it is WARNED here rather than left to
    /// "the caller", which no caller did. A file that cannot be removed still
    /// holds a live token, so the log line is the only thing that can bring an
    /// operator to it before [`reap_orphans_in`] does.
    /// Test: `spec_round_trips_through_the_file`, `consume_removes_the_file`,
    /// `consume_reports_a_missing_spec`, `consume_reports_a_corrupt_spec`.
    pub fn consume(path: &Path) -> Result<Self, LaunchSpecError> {
        let bytes = std::fs::read(path).map_err(|source| LaunchSpecError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if let Err(err) = std::fs::remove_file(path) {
            tracing::warn!(
                spec = %path.display(),
                "launch spec could not be deleted after reading it — it still holds this \
                 session's credentials and will only go away when a later launch reaps it \
                 (#8233): {err}"
            );
        }
        serde_json::from_slice(&bytes).map_err(|source| LaunchSpecError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The sentinel path proving the pane's shell RAN one SPECIFIC launch.
    ///
    /// Why: see [`STARTED_SUFFIX`]. Both sides have to compute the same path
    /// from what they each know — the shim from the spec it just consumed, the
    /// daemon from [`LaunchSpec::read_launch_pointer_in`] — so the derivation
    /// lives in one function keyed on the LAUNCH id.
    /// What: `<dir>/<launch_id>.started`.
    /// Test: `a_marker_from_an_earlier_launch_does_not_satisfy_a_later_one`
    /// (`launch_verify.rs`), `deliver_clears_a_stale_started_sentinel`
    /// (`managed_launch_tests.rs`).
    pub fn started_marker_in(dir: &Path, launch_id: &str) -> PathBuf {
        dir.join(format!("{launch_id}{STARTED_SUFFIX}"))
    }

    /// The per-session pointer file naming the launch currently in flight.
    ///
    /// Why: see [`LAUNCH_POINTER_SUFFIX`].
    /// What: `<dir>/<session_id>.launch`.
    /// Test: `deliver_clears_a_stale_started_sentinel` (`managed_launch_tests.rs`)
    /// writes and reads one back through this derivation.
    pub fn launch_pointer_in(dir: &Path, session_id: &str) -> PathBuf {
        dir.join(format!("{session_id}{LAUNCH_POINTER_SUFFIX}"))
    }

    /// Publish this spec's launch id as the session's current launch.
    ///
    /// Why: written by the launcher immediately before the keystrokes, so a
    /// checker that reads it afterwards can only ever be looking at THIS launch.
    /// What: overwrites `<dir>/<session_id>.launch` with the launch id. Errors
    /// are returned, not swallowed: without the pointer the checker cannot tell
    /// a delivered launch from an undelivered one, so the launch is abandoned
    /// rather than made unverifiable.
    /// Test: `deliver_clears_a_stale_started_sentinel` (`managed_launch_tests.rs`)
    /// — delivery publishes this launch's id and the test reads it back.
    pub fn write_launch_pointer_in(&self, dir: &Path) -> Result<PathBuf, LaunchSpecError> {
        let path = Self::launch_pointer_in(dir, &self.session_id);
        std::fs::write(&path, self.launch_id.as_bytes()).map_err(|source| {
            LaunchSpecError::Write {
                path: path.clone(),
                source,
            }
        })?;
        Ok(path)
    }

    /// Read back the launch id the session's pointer names.
    ///
    /// Why: the daemon's post-send check knows only the session record.
    /// What: `None` when no pointer exists or it cannot be read — which the
    /// caller must treat as "cannot tell", never as "the launch failed".
    /// Test: `deliver_clears_a_stale_started_sentinel` (`managed_launch_tests.rs`)
    /// for the round trip; `delivery_is_assumed_without_a_launch_pointer`
    /// (`launch_verify.rs`) for the `None` arm.
    pub fn read_launch_pointer_in(dir: &Path, session_id: &str) -> Option<String> {
        let raw = std::fs::read_to_string(Self::launch_pointer_in(dir, session_id)).ok()?;
        let id = raw.trim().to_owned();
        (!id.is_empty()).then_some(id)
    }

    /// Record that the pane's shell reached this shim (#8233 review).
    ///
    /// Why: this is the sentinel the launcher waits on. It is written BEFORE
    /// `claude` is spawned, so it separates "the shell never ran the line" — the
    /// stuck-parser failure this issue is about — from "the line ran and the
    /// launch then failed", which the pane's own output already explains.
    /// What: creates `<spec dir>/<launch_id>.started`, empty. Best-effort: a
    /// launch must not be abandoned because a marker could not be written, but
    /// the failure is warned because it will read downstream as a stuck pane.
    /// Test: `mark_started_writes_the_sentinel_beside_the_spec`.
    pub fn mark_started(spec_path: &Path, launch_id: &str) {
        let dir = spec_path.parent().unwrap_or(Path::new("."));
        let marker = Self::started_marker_in(dir, launch_id);
        if let Err(err) = std::fs::write(&marker, b"") {
            tracing::warn!(
                marker = %marker.display(),
                "could not write the launch-started sentinel; the daemon will read this \
                 launch as one the pane never ran (#8233): {err}"
            );
        }
    }

    /// Read a spec back WITHOUT consuming it, to prove it is usable.
    ///
    /// Why: the daemon writes the spec and then types a line naming it, and by
    /// then the launch is out of its hands. Reading it back first turns "the
    /// carrier is missing, unreadable or corrupt" into a spawn error the caller
    /// can mark the session record errored on, instead of a pane that quietly
    /// sits at a bare shell (#8233 fail-closed requirement).
    /// What: the same read-and-decode [`LaunchSpec::consume`] performs, minus the
    /// removal. Returns the decoded spec so a caller can also assert on it.
    /// Test: `verify_accepts_a_spec_just_written`, `verify_reports_a_corrupt_spec`.
    pub fn verify(path: &Path) -> Result<Self, LaunchSpecError> {
        let bytes = std::fs::read(path).map_err(|source| LaunchSpecError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| LaunchSpecError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Build the [`std::process::Command`] this spec describes.
    ///
    /// Why: this is where "the claude the OS sees" is decided — cwd, env and
    /// argv all come from the spec rather than from a shell that re-splits a
    /// string, so nothing about the launch depends on pane-shell quoting.
    /// What: `program` + `args`, `current_dir(cwd)`, one `env_remove` per
    /// `env_unset` entry FIRST (mirroring POSIX `env -u` preceding every
    /// assignment), then one `env` per `env_set` pair, then
    /// `TM_MANAGED_SESSION_ID` (#2023 component B) and the
    /// `core::alt_screen` managed defaults, which yield per-variable to a value
    /// the pane already exports exactly as the `${NAME-1}` shell form did
    /// (#6495/#7160).
    /// Test: `spec_command_carries_cwd_program_argv_and_env`,
    /// `spec_command_yields_the_alt_screen_default_to_the_pane`.
    pub fn to_command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.args).current_dir(&self.cwd);
        for name in &self.env_unset {
            cmd.env_remove(name);
        }
        for (name, value) in &self.env_set {
            cmd.env(name, value);
        }
        cmd.env(
            crate::core::harness_root::MANAGED_SESSION_ID_ENV,
            &self.session_id,
        );
        // #6495/#7160: the `${NAME-1}` shell operands became this call — same
        // table, same per-variable operator precedence, no shell needed.
        crate::core::alt_screen::apply_default_to_command(&mut cmd);
        cmd
    }
}

/// Sweep the production spec root of everything an earlier launch abandoned.
///
/// Why (#8233 review round 2, HIGH): [`LaunchSpec::write_in`] reaps, but only
/// when a NEXT launch happens. A fleet that stops launching — the daemon
/// restarted, every session already up, the operator away for the weekend —
/// leaves the last abandoned spec, and its `GH_TOKEN` and
/// `CLAUDE_CODE_OAUTH_TOKEN`, on disk indefinitely. Exposing the sweep lets the
/// supervisor's own heartbeat run it, which makes reaping depend on the daemon
/// being alive rather than on a launch arriving.
/// What: [`reap_orphans_in`] against [`LaunchSpec::root`], with the same TTL.
/// Silent and non-fatal when the root cannot be resolved or does not exist.
/// Test: `reap_survives_a_missing_directory` for the absent-root arm, and
/// `write_reaps_an_orphaned_spec` for the sweep itself.
pub fn reap_orphans() {
    if let Some(dir) = LaunchSpec::root() {
        reap_orphans_in(&dir, ORPHAN_TTL);
    }
}

/// Delete every entry in `dir` older than `ttl` (#8233 review, HIGH).
///
/// Why: a spec is consumed-and-deleted by the shim within about a second, and
/// [`super::managed_launch::deliver`] removes the one it wrote on every abort
/// path. Neither covers the cases where the daemon is no longer in the loop — a
/// pane killed between the write and the keystroke, a daemon restart, a launch
/// the shell never ran. The file left behind holds `GH_TOKEN` and
/// `CLAUDE_CODE_OAUTH_TOKEN` in cleartext, and this directory (unlike the
/// `TMPDIR` file it replaced) is never swept by the OS.
/// What: best-effort, non-fatal, and deliberately dumb — anything in the spec
/// directory whose mtime is older than `ttl` goes, specs and `.started`
/// sentinels alike. An entry whose metadata or mtime cannot be read is LEFT
/// alone rather than guessed at. Errors are logged, never propagated: reaping is
/// hygiene, and a launch must not fail because an old file would not delete.
/// Test: `write_reaps_an_orphaned_spec`, `write_keeps_a_fresh_spec`,
/// `reap_survives_a_missing_directory`.
fn reap_orphans_in(dir: &Path, ttl: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().map(|age| age > ttl).unwrap_or(false))
            .unwrap_or(false);
        if !stale {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::warn!(
                orphan = %path.display(),
                "reaped an abandoned launch spec — its pane never consumed it, so a \
                 launch was lost and its credentials sat on disk until now (#8233)"
            ),
            Err(err) => tracing::warn!(
                orphan = %path.display(),
                "abandoned launch spec could not be reaped (#8233): {err}"
            ),
        }
    }
}

/// Create `dir` (and its parents) with owner-only permissions.
///
/// Why: the spec files inside carry credentials, so the directory must not be
/// traversable by another local user. Setting the mode on the directory as well
/// as the file is defence in depth against an umask that would otherwise leave
/// it 0755.
/// What: `create_dir_all`, then `set_permissions(0700)` on unix. The mode is set
/// on the DIRECTORY (not a file), so there is no window in which a secret is
/// readable — the files inside are created 0600 from the start.
/// Test: `write_creates_an_owner_only_file_in_an_owner_only_dir`.
fn ensure_owner_only_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))?;
    }
    Ok(())
}

/// Write `data` to a NEW file created with mode 0600.
///
/// Why: `std::fs::write` honours the umask, so a spec carrying `GH_TOKEN` could
/// land group-readable; and a `chmod` after the write leaves a window in which
/// it was. Opening with the mode closes both.
/// What: `OpenOptions::create_new(true).mode(0600)` on unix — `create_new` also
/// means a uuid collision errors instead of clobbering another live launch.
/// Test: `write_creates_an_owner_only_file_in_an_owner_only_dir`.
fn create_owner_only(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(FILE_MODE);
    }
    let mut file = options.open(path)?;
    file.write_all(data)
}

#[cfg(test)]
#[path = "launch_spec_tests.rs"]
mod tests;
