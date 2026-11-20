//! The `com.trusty.browser` launchd job (`cli.py` `launch_configuration`, `manage`).
//!
//! Why: the broker must start at login and stay up; Python's `install-service`
//! refuses any plist or launcher it did not write, so the Rust binary owns the
//! same files and, once, accepts the exact files Python 0.7.0 wrote (plan
//! section 5, cutover step 5), so `uninstall-service`/`install-service` can
//! replace the Python registration without hand edits.
//! What: [`plist`] renders `plistlib.dumps` output for any argv; the Rust job
//! runs `<exe> --state <dir> serve`, the argv the native host pins
//! ([`crate::peer_darwin::service_argv_ok`]). [`manage`] is Python's `manage`
//! with the `launchctl` calls behind [`Launchctl`] so tests never reach the
//! real `gui/<uid>` domain.
//!
//! Signing: install runs [`crate::codesign::ensure_install_signed`] on `exe` and its MCP sibling after
//! the ownership checks and before any file is written, so a failed sign or
//! verify leaves no plist or manifest behind (#11440).
//! Test: `python_plist_renders_byte_identically`, `install_service_writes_files_and_bootstraps`,
//! `uninstall_service_replaces_a_python_registration` in `tests/install.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::codesign::{Signer, ensure_install_signed};
use crate::install_host::{self, Registration};

/// The launchd label, from the shared registry (#4919: no label literal outside it).
pub const LABEL: &str = trusty_common::launchd_labels::BROWSER;

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

/// `plistlib` string escaping; control characters are refused as plistlib does.
fn escape(s: &str) -> io::Result<String> {
    if s.chars()
        .any(|c| (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(io::Error::other(
            "strings can't contain control characters; use bytes instead",
        ));
    }
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    Ok(s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}

/// `plistlib.dumps(launch_configuration)` for `argv` and `state`.
pub fn plist(argv: &[&str], state: &Path) -> io::Result<String> {
    let mut args = String::new();
    for a in argv {
        args.push_str(&format!("\t\t<string>{}</string>\n", escape(a)?));
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

/// The Rust job's argv: `<exe> --state <state> serve`.
pub fn service_args(exe: &Path, state: &Path) -> Vec<String> {
    vec![
        exe.to_string_lossy().into_owned(),
        "--state".to_owned(),
        state.to_string_lossy().into_owned(),
        "serve".to_owned(),
    ]
}

/// `launch_configuration`: the plist the Rust job installs.
pub fn launch_configuration(exe: &Path, state: &Path) -> io::Result<String> {
    let argv = service_args(exe, state);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    plist(&refs, state)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The Python 0.7.0 plist: `[python, .../trusty_browser/service.py, --state, <state>, serve]`.
fn python_plist(text: &str, state: &Path) -> bool {
    let argv: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("\t\t<string>")?.strip_suffix("</string>"))
        .map(unescape)
        .collect();
    let [py, script, ..] = argv.as_slice() else {
        return false;
    };
    let st = state.to_string_lossy();
    script.ends_with("/trusty_browser/service.py")
        && plist(&[py, script, "--state", &st, "serve"], state).is_ok_and(|p| p == text)
}

/// The Python 0.7.0 launcher: `exec <python> .../trusty_browser/native_host.py --socket <sock> "$@"`.
fn python_launcher(text: &str, sock: &Path) -> bool {
    let sock = sock.to_string_lossy();
    let tail = format!(" {} \"$@\"\n", install_host::shjoin(&["--socket", &sock]));
    let Some(mid) = text
        .strip_prefix("#!/bin/sh\nexec ")
        .and_then(|t| t.strip_suffix(&tail))
    else {
        return false;
    };
    let words: Vec<&str> = mid.split(' ').collect();
    match words.as_slice() {
        [py, host] => {
            host.ends_with("/trusty_browser/native_host.py")
                && install_host::shquote(py) == *py
                && install_host::shquote(host) == *host
        }
        _ => false,
    }
}

/// How an existing registration file relates to ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// Nothing at the path.
    Absent,
    /// Exactly what we would write.
    Ours,
    /// Exactly what Python 0.7.0 wrote; replaced on install, removed on uninstall.
    Python,
}

/// `installed_content`, extended with the one-time Python match.
///
/// `legacy` decides whether a file's text is Python's version of it.
pub fn installed(
    path: &Path,
    content: &str,
    mode: u32,
    legacy: &dyn Fn(&str) -> bool,
) -> io::Result<Presence> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Presence::Absent),
        Err(e) => return Err(e),
        Ok(m) => m,
    };
    if install_host::same_file(path, content, mode) {
        return Ok(Presence::Ours);
    }
    let text = fs::read_to_string(path).unwrap_or_default();
    // A file that is not ours must still be a regular owner-controlled one.
    if meta.is_file() && install_host::same_file(path, &text, mode) && legacy(&text) {
        return Ok(Presence::Python);
    }
    Err(io::Error::other(format!(
        "Refusing to change an unrelated registration: {}",
        path.display()
    )))
}

/// Where `manage` reads and writes; tests point every path at a temp HOME.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The user's home (LaunchAgents and the Chrome manifests live under it).
    pub home: PathBuf,
    /// The resolved state directory.
    pub state: PathBuf,
    /// The canonical path of the `trusty-browser` binary the job runs.
    pub exe: PathBuf,
    /// The current uid (the `gui/<uid>` domain).
    pub uid: u32,
}

fn legacy_for(r: &Registration, state: &Path) -> Box<dyn Fn(&str) -> bool> {
    let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned());
    let sock = match name.as_deref() {
        Some("chrome-native-host") => Some(state.join("service.sock")),
        Some("chrome-native-host.enroll") => Some(state.join("enrollment.sock")),
        // Manifests are identical in both ports: they name the same launcher.
        _ => None,
    };
    match sock {
        Some(sock) => Box::new(move |t| python_launcher(t, &sock)),
        None => Box::new(|_| false),
    }
}

/// `install-service` (`install = true`) or `uninstall-service`.
///
/// Why: one routine keeps Python's ownership checks for both verbs.
/// What: refuses a plist or registration file it did not write (except
/// Python's own, see [`Presence::Python`]) and a loaded job whose plist is
/// elsewhere. Install: LaunchAgents 0700, state 0700, publish the extension,
/// the host registrations, the plist (0600), then `bootstrap` unless already
/// loaded. Uninstall: `bootout` when loaded, then remove every owned file.
/// Test: `install_service_writes_files_and_bootstraps`, `uninstall_service_replaces_a_python_registration`,
/// `install_service_refuses_a_job_owned_elsewhere`, `install_service_writes_nothing_when_signing_fails`.
pub fn manage(
    layout: &Layout,
    install: bool,
    ctl: &dyn Launchctl,
    signer: &Signer<'_>,
) -> Result<(), LaunchdError> {
    if !cfg!(target_os = "macos") {
        return Err(io::Error::other("launchd service installation requires macOS").into());
    }
    let state = &layout.state;
    let domain = format!("gui/{}", layout.uid);
    let target = format!("{domain}/{LABEL}");
    let agents = layout.home.join("Library/LaunchAgents");
    let plist_path = agents.join(format!("{LABEL}.plist"));
    let manifests = install_host::default_directory(&layout.home)?;
    let registrations = install_host::registration_files(state, &manifests, &layout.exe);
    let configuration = launch_configuration(&layout.exe, state)?;
    let st = state.clone();
    let present = installed(&plist_path, &configuration, 0o600, &move |t| {
        python_plist(t, &st)
    })?;
    let loaded = ctl.run(&["print", &target])?;
    let marker = format!("path = {}\n", plist_path.display());
    if loaded.code == 0 && (present == Presence::Absent || !loaded.stdout.contains(&marker)) {
        return Err(io::Error::other(format!("A different launchd job owns {LABEL}")).into());
    }
    let mut owned = Vec::new();
    for r in &registrations {
        let p = installed(&r.path, &r.content, r.mode, &*legacy_for(r, state))?;
        if p != Presence::Absent {
            owned.push((r, p));
        }
    }
    if install {
        // #11440: sign (or refuse) before writing anything.
        ensure_install_signed(&layout.exe, signer)?;
        install_host::mkdir_0700(&agents)?;
        install_host::mkdir_0700(state)?;
        crate::assets::extension_path(state)?;
        for (r, p) in &owned {
            if *p == Presence::Python {
                install_host::replace_owned(&r.path, &r.content, r.mode)?;
            }
        }
        install_host::install(state, &manifests, &layout.exe)?;
        if present == Presence::Python {
            install_host::replace_owned(&plist_path, &configuration, 0o600)?;
        }
        install_host::write_owned(&plist_path, &configuration, 0o600)?;
        // #11440: a loaded Python job keeps running its old plist until booted out.
        if loaded.code == 0 && present == Presence::Python {
            checked(ctl, &["bootout", &target])?;
        }
        if loaded.code != 0 || present == Presence::Python {
            checked(ctl, &["bootstrap", &domain, &plist_path.to_string_lossy()])?;
        }
    } else {
        if loaded.code == 0 {
            checked(ctl, &["bootout", &target])?;
        }
        if present != Presence::Absent {
            fs::remove_file(&plist_path)?;
        }
        for (r, _) in owned {
            fs::remove_file(&r.path)?;
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
