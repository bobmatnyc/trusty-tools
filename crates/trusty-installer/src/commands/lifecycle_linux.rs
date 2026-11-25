//! Linux `tctl restart`: install-dir binaries and the running-version check
//! (#11086, phase 3 of #11083).
//!
//! Why: on Linux, `tctl restart` routed every launchd member to the
//! `launchd_control` stub and failed, and its one working member ran
//! `trusty-mpm restart` from `PATH`. A daemon could keep running a binary that
//! `tctl upgrade` had already replaced, and nothing reported it.
//! What: the pieces the restart loop and the `Launchd` arm of
//! `apply_to_member_in` use: which members ship Linux lifecycle verbs, which
//! binary speaks for a member, the `<install dir>/<binary> restart` spawn, the
//! installed-version read, and the running-versus-installed verdict. The host
//! reads (install dir, health probe) sit behind [`RestartHost`] so tests drive
//! them with stub binaries in a temp dir.
//! Test: `lifecycle_linux_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::commands::probe_http::ProbeOutcome;
use crate::commands::stable_set::ManageStrategy;

/// Launchd members whose binaries ship Linux `start`/`stop`/`restart` verbs
/// (#11085). trusty-mpm restarts through its `OwnVerb` arm instead.
const LINUX_VERB_MEMBERS: [&str; 3] = ["trusty-search", "trusty-memory", "trusty-console"];

/// Does this launchd member ship its own Linux `restart` verb?
pub(super) fn has_linux_verbs(binary: &str) -> bool {
    LINUX_VERB_MEMBERS.contains(&binary)
}

/// The binary that restarts and reports the version for a member: `tm` for
/// trusty-mpm, whose member binary is the `trusty-mpm` shim; the member binary
/// otherwise.
pub(super) fn daemon_binary(binary: &str) -> &str {
    if binary == "trusty-mpm" {
        "tm"
    } else {
        binary
    }
}

/// The host reads a Linux restart needs.
///
/// Why: the install dir and the health probe are both host state; a test
/// points them at a temp dir and a canned probe answer.
/// What: `install_dir` is where tctl installs binaries; `probe_health` is the
/// running daemon's health answer.
/// Test: `FakeRestartHost` in `lifecycle_tests.rs` implements it for every
/// lifecycle test.
pub(super) trait RestartHost {
    /// The directory tctl installs binaries into.
    fn install_dir(&self) -> PathBuf;
    /// The running daemon's health answer for this member.
    fn probe_health(&self, binary: &str, manage: ManageStrategy) -> ProbeOutcome;
}

/// The real host: [`crate::download::install_dir_or_fallback`] and
/// [`crate::commands::probe::probe_member_health`].
pub(super) struct RealRestartHost;

impl RestartHost for RealRestartHost {
    fn install_dir(&self) -> PathBuf {
        crate::download::install_dir_or_fallback()
    }

    fn probe_health(&self, binary: &str, manage: ManageStrategy) -> ProbeOutcome {
        crate::commands::probe::probe_member_health(binary, manage)
    }
}

/// Run `<exe> restart` and map its exit status.
///
/// Why: the daemon's own `restart` verb is stop, confirm stopped, then start
/// (D1), so tctl only invokes it, and by absolute path so a copy earlier on
/// `PATH` is never run.
/// What: refuses an `exe` that is not a file. The child gets no stdin, and its
/// stdout goes to tctl's stderr so `--json` output stays clean; neither is a
/// pipe tctl waits on, so the detached daemon cannot hold tctl open. A nonzero
/// exit is an `Err` that names the exit status.
/// Test: `linux_restart_runs_the_install_dir_binary_not_the_path_decoy`,
/// `rejected_restart_is_failed_and_later_members_still_restart`.
pub(super) fn run_restart_verb(exe: &Path) -> anyhow::Result<String> {
    if !exe.is_file() {
        anyhow::bail!("{} is not in the install directory", exe.display());
    }
    let status = Command::new(exe)
        .arg("restart")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{} restart`: {e}", exe.display()))?;
    if status.success() {
        Ok(format!("{} restart ok", exe.display()))
    } else {
        anyhow::bail!(
            "`{} restart` exited with {status} (a binary older than the Linux \
             `restart` verb rejects it)",
            exe.display()
        )
    }
}

/// The version the install-dir binary reports through `--version`.
///
/// Why: `~/.cargo/.crates.toml` can record a version long gone from disk, so
/// the binary itself is the only source for what is installed.
/// What: runs `<exe> --version` through the 10 s timeout of
/// `trusty_common::update::verify_installed_binary_at_path`, on a fresh
/// current-thread runtime, and extracts the version token.
/// Test: `installed_version_comes_from_the_binary_not_crates_toml`,
/// `mpm_installed_version_comes_from_tm_not_the_shim`.
pub(super) fn installed_version(exe: &Path) -> anyhow::Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("cannot build a runtime to read the version: {e}"))?;
    let raw = rt.block_on(trusty_common::update::verify_installed_binary_at_path(exe))?;
    crate::commands::update_engine::extract_version_from_line(&raw).ok_or_else(|| {
        anyhow::anyhow!(
            "`{} --version` printed no version: {}",
            exe.display(),
            raw.trim()
        )
    })
}

/// Decide a restarted member's outcome from the installed and running versions.
///
/// Why: a `restart` that exits 0 proves nothing about what now runs (AC4, AC5).
/// What: `Ok` only when the health reply carries the installed version, with
/// both versions in the detail. A different version, a reply with no version,
/// or any other probe answer is `Err` naming the installed version.
/// Test: `running_version_mismatch_is_failed_with_both_versions`,
/// `refused_health_probe_after_restart_is_failed`.
pub(super) fn verdict(
    restarted: &str,
    installed: &str,
    health: &ProbeOutcome,
) -> Result<String, String> {
    match health {
        ProbeOutcome::Serving {
            version: Some(running),
            ..
        } if running.trim().trim_start_matches('v') == installed => Ok(format!(
            "{restarted}; running {running}, installed {installed}"
        )),
        ProbeOutcome::Serving {
            version: Some(running),
            ..
        } => Err(format!(
            "restart exited 0 but the daemon runs {running}; installed is {installed}"
        )),
        ProbeOutcome::Refused => Err(format!(
            "restart exited 0 but the health probe was refused; installed is {installed}"
        )),
        other => Err(format!(
            "restart exited 0 but the health probe answered {other:?}; installed is {installed}"
        )),
    }
}
