//! The `com.trusty.computer` launchd job: `install-service` and `uninstall-service`.
//!
//! Why: the broker must start at login and stay up, and the job must run the
//! exact signed binary the operator installed (plan section 7, issue #11460;
//! port of trusty-browser `launchd.rs` and `install_host.rs`, trimmed to the
//! plist and its file helpers).
//! What: [`plist`] renders the job for `<exe> broker --state <dir>`. [`manage`]
//! refuses a test build, a plist it did not write and a loaded job whose plist
//! is elsewhere; install then signs both binaries, checks the CuaDriver
//! version, and only then writes the plist (0600) and bootstraps. The
//! `launchctl` calls sit behind [`Launchctl`], so tests never reach the real
//! `gui/<uid>` domain.
//! Test: `install_service_writes_files_and_bootstraps`, `uninstall_service_removes_the_job`
//! in `tests/install.rs`.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::codesign::{Signer, ensure_install_signed};

/// The launchd label.
pub const LABEL: &str = "com.trusty.computer";

/// Why a launchd action failed.
#[derive(Debug, thiserror::Error)]
pub enum LaunchdError {
    /// A refusal or I/O error; the CLI prints the message.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// `launchctl` exited non-zero on a checked call.
    #[error("launchctl failed: {0}")]
    Launchctl(String),
}

/// One `launchctl` result.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// Exit status (0 = success).
    pub code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Runs `/bin/launchctl <args>`; a seam so tests never touch launchd.
pub trait Launchctl {
    /// Run once and capture the result.
    fn run(&self, args: &[&str]) -> io::Result<Outcome>;
}

/// The real `/bin/launchctl`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLaunchctl;

impl Launchctl for SystemLaunchctl {
    fn run(&self, args: &[&str]) -> io::Result<Outcome> {
        let out = std::process::Command::new("/bin/launchctl")
            .args(args)
            .output()?;
        Ok(Outcome {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Plist string escaping; control characters are refused.
fn escape(s: &str) -> io::Result<String> {
    if s.chars()
        .any(|c| (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(io::Error::other(
            "plist strings can't contain control characters",
        ));
    }
    Ok(s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}

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
    let mut args = String::new();
    for a in service_args(exe, state) {
        args.push_str(&format!("\t\t<string>{}</string>\n", escape(&a)?));
    }
    let err = escape(&state.join("service.stderr.log").to_string_lossy())?;
    let out = escape(&state.join("service.stdout.log").to_string_lossy())?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>KeepAlive</key>\n\t<true/>\n\
\t<key>Label</key>\n\t<string>{LABEL}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n{args}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
\t<key>StandardErrorPath</key>\n\t<string>{err}</string>\n\
\t<key>StandardOutPath</key>\n\t<string>{out}</string>\n\
\t<key>Umask</key>\n\t<integer>63</integer>\n\
</dict>\n</plist>\n"
    ))
}

/// True when `path` is a regular, user-owned file with exactly `mode` and `content`.
pub fn same_file(path: &Path, content: &str, mode: u32) -> bool {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            m.is_file()
                && m.uid() == crate::private::uid()
                && m.mode() & 0o777 == mode
                && fs::read(path).is_ok_and(|b| b == content.as_bytes())
        }
        Err(_) => false,
    }
}

/// Publish a complete file once; an identical owned file is a no-op.
///
/// What: writes an `O_EXCL` temp file with `mode` beside `path` and hard-links
/// it into place, so a reader never sees a partial file and a different
/// existing file is never replaced (`existing file differs`).
pub fn write_owned(path: &Path, content: &str, mode: u32) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    let linked = f
        .write_all(content.as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| fs::hard_link(&tmp, path));
    let _ = fs::remove_file(&tmp);
    match linked {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && !same_file(path, content, mode) => {
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("existing file differs: {}", path.display()),
            ))
        }
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

/// `mkdir -p` the parents, then the leaf with mode 0700.
fn mkdir_0700(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
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

fn checked(ctl: &dyn Launchctl, args: &[&str]) -> Result<(), LaunchdError> {
    let out = ctl.run(args)?;
    if out.code == 0 {
        Ok(())
    } else {
        Err(LaunchdError::Launchctl(out.stderr.trim().to_owned()))
    }
}
