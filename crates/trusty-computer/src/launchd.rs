//! The launchd job for trusty-computer: `install-service` and `uninstall-service`.
//!
//! Why: the broker must start at login and stay up, and the job must run the
//! exact signed binary the operator installed (plan section 7, issue #11460;
//! the plist writer and file helpers are shared with trusty-browser through
//! `trusty-daemon-common`, #11692).
//! What: [`plist`] renders the job for `<exe> broker --state <dir>`. [`manage`]
//! refuses a test build, a plist it did not write and a loaded job whose plist
//! is elsewhere; install then signs both binaries, checks the CuaDriver
//! version, and only then writes the plist (0600) and bootstraps. The
//! `launchctl` calls sit behind [`Launchctl`], so tests never reach the real
//! `gui/<uid>` domain.
//! Test: `install_service_writes_files_and_bootstraps`, `uninstall_service_removes_the_job`
//! in `tests/install.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::codesign::{Signer, ensure_install_signed};
// #11692: the plist writer, `launchctl` seam and owned-file helpers are shared.
pub use trusty_daemon_common::launchd::{
    Launchctl, LaunchdError, Outcome, SystemLaunchctl, same_file, write_owned,
};
use trusty_daemon_common::launchd::{checked, mkdir_0700};

/// The launchd label.
pub const LABEL: &str = trusty_common::launchd_labels::COMPUTER;

/// The job's argv: `<exe> broker --state <state>`.
pub fn service_args(exe: &Path, state: &Path) -> Vec<String> {
    vec![
        exe.to_string_lossy().into_owned(),
        "broker".to_owned(),
        "--state".to_owned(),
        state.to_string_lossy().into_owned(),
    ]
}

/// The plist for `exe` and `state`: KeepAlive, RunAtLoad, logs in `state`, umask 077.
pub fn plist(exe: &Path, state: &Path) -> io::Result<String> {
    let argv = service_args(exe, state);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    trusty_daemon_common::launchd::plist(LABEL, &refs, state)
}

/// Where `manage` reads and writes; tests point every path at a temp HOME.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The user's home (LaunchAgents lives under it).
    pub home: PathBuf,
    /// The resolved state directory.
    pub state: PathBuf,
    /// The canonical path of the `trusty-computer` binary the job runs.
    pub exe: PathBuf,
    /// The current uid (the `gui/<uid>` domain).
    pub uid: u32,
}

/// The plist path under `home`.
pub fn plist_path(home: &Path) -> PathBuf {
    home.join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

/// Refuse registration from a build with the `test-seams` feature.
///
/// Why: a test-seams build may carry test-only hooks; it must never become
/// the login job (plan section 8).
/// Test: `test_seams_build_refuses_to_register`.
pub fn refuse_test_build(test_seams: bool) -> io::Result<()> {
    if test_seams {
        return Err(io::Error::other(
            "this trusty-computer was built with the test-seams feature; it never registers a launchd job",
        ));
    }
    Ok(())
}

/// Refuse any installed CuaDriver other than the pinned release.
///
/// Test: `install_service_refuses_the_wrong_cua_driver`.
pub fn check_driver_version(version: Result<String, String>) -> io::Result<()> {
    let v =
        version.map_err(|e| io::Error::other(format!("cannot read the CuaDriver version: {e}")))?;
    if v.split_whitespace().last() != Some(crate::launcher::DRIVER_VERSION) {
        return Err(io::Error::other(format!(
            "CuaDriver {} is required at {}; found {v:?}",
            crate::launcher::DRIVER_VERSION,
            crate::launcher::APP_PATH
        )));
    }
    Ok(())
}

/// `install-service` (`install = true`) or `uninstall-service`.
///
/// Why: one routine keeps the ownership checks for both verbs, and install
/// fails closed: every check runs before the first write.
/// What: refuses a test build, a plist at [`plist_path`] it did not write,
/// and a loaded job whose plist is elsewhere. Install: sign both binaries
/// ([`ensure_install_signed`]), check `driver_version`, then state 0700,
/// LaunchAgents, the plist (0600), and `bootstrap` unless already loaded.
/// Uninstall: `bootout` when loaded, then remove the plist.
/// Test: `install_service_writes_files_and_bootstraps`, `uninstall_service_removes_the_job`,
/// `install_service_refuses_a_job_owned_elsewhere`, `install_service_writes_nothing_when_signing_fails`.
pub fn manage(
    layout: &Layout,
    install: bool,
    ctl: &dyn Launchctl,
    signer: &Signer<'_>,
    driver_version: &dyn Fn() -> Result<String, String>,
) -> Result<(), LaunchdError> {
    refuse_test_build(cfg!(feature = "test-seams"))?;
    if !cfg!(target_os = "macos") {
        return Err(io::Error::other("launchd service installation requires macOS").into());
    }
    let state = &layout.state;
    let domain = format!("gui/{}", layout.uid);
    let target = format!("{domain}/{LABEL}");
    let path = plist_path(&layout.home);
    let configuration = plist(&layout.exe, state)?;
    let present = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
        Ok(_) if same_file(&path, &configuration, 0o600) => true,
        Ok(_) => {
            return Err(io::Error::other(format!(
                "Refusing to change an unrelated registration: {}",
                path.display()
            ))
            .into());
        }
    };
    let loaded = ctl.run(&["print", &target])?;
    let marker = format!("path = {}\n", path.display());
    if loaded.code == 0 && (!present || !loaded.stdout.contains(&marker)) {
        return Err(io::Error::other(format!("A different launchd job owns {LABEL}")).into());
    }
    if install {
        // #11460: sign (or refuse) and check the driver before writing anything.
        ensure_install_signed(&layout.exe, signer)?;
        check_driver_version(driver_version())?;
        crate::private::ensure_private_dir(state)?;
        mkdir_0700(path.parent().unwrap_or(&layout.home))?;
        write_owned(&path, &configuration, 0o600)?;
        if loaded.code != 0 {
            checked(ctl, &["bootstrap", &domain, &path.to_string_lossy()])?;
        }
    } else {
        if loaded.code == 0 {
            checked(ctl, &["bootout", &target])?;
        }
        if present {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}
