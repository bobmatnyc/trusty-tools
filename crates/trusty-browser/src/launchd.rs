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

// #11692: the plist writer and `launchctl` seam are shared with trusty-computer.
use trusty_daemon_common::launchd::checked;
pub use trusty_daemon_common::launchd::{Launchctl, LaunchdError, Outcome, SystemLaunchctl};

/// `plistlib.dumps(launch_configuration)` for `argv` and `state`.
pub fn plist(argv: &[&str], state: &Path) -> io::Result<String> {
    trusty_daemon_common::launchd::plist(LABEL, argv, state)
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
    /// An earlier registration of ours, replaced on install and removed on
    /// uninstall: exactly what Python 0.7.0 wrote, or (#11927) a pre-#11927
    /// launcher, or a launcher or plist whose binary is gone.
    Previous,
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
        return Ok(Presence::Previous);
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

fn legacy_for(r: &Registration, exe: &Path) -> Box<dyn Fn(&str) -> bool> {
    // Manifests are identical in both ports: they name the same launcher.
    match install_host::launcher_socket(&r.path) {
        // #11927: also an earlier Rust launcher, or one whose binary is gone.
        Some(sock) => {
            let exe = exe.to_path_buf();
            Box::new(move |t| {
                python_launcher(t, &sock) || install_host::previous_launcher(t, &sock, &exe)
            })
        }
        None => Box::new(|_| false),
    }
}

/// Why: `install-service` from a stable binary must replace the plist a
/// removed build binary left, or the repair the popup names refuses (#11927).
/// What: true when `text` is exactly our plist for the binary it names and
/// `state`, and that binary is [`install_host::binary_gone`].
/// Test: `install_service_replaces_a_registration_whose_binary_is_gone`,
/// `install_service_refuses_our_plist_for_another_live_binary` in `tests/install.rs`.
fn stale_plist(text: &str, state: &Path) -> bool {
    let Some(old) = text
        .lines()
        .find_map(|l| l.strip_prefix("\t\t<string>")?.strip_suffix("</string>"))
        .map(unescape)
    else {
        return false;
    };
    let old = Path::new(&old);
    // #11927 review: only NotFound is gone; an unreadable binary is not.
    install_host::binary_gone(old) && launch_configuration(old, state).is_ok_and(|p| p == text)
}

/// `install-service` (`install = true`) or `uninstall-service`.
///
/// Why: one routine keeps Python's ownership checks for both verbs.
/// What: refuses a plist or registration file it did not write, except an
/// earlier registration of ours ([`Presence::Previous`]: Python 0.7.0's, the
/// 0.8.2 launcher, or one whose binary is gone), and a loaded job whose plist is
/// elsewhere. Install: LaunchAgents 0700, state 0700, publish the extension,
/// the host registrations, the plist (0600), then `bootstrap`, or
/// `kickstart -k` when the job is already loaded (#11522). Uninstall: `bootout` when loaded, then remove every owned file.
/// Test: `install_service_writes_files_and_bootstraps`, `uninstall_service_replaces_a_python_registration`,
/// `install_service_refuses_a_job_owned_elsewhere`, `install_service_writes_nothing_when_signing_fails`,
/// `install_over_a_loaded_broker_restarts_it`, `install_over_a_loaded_broker_reports_a_failed_restart`.
pub fn manage(
    layout: &Layout,
    install: bool,
    ctl: &dyn Launchctl,
    signer: &Signer<'_>,
) -> Result<(), LaunchdError> {
    manage_with(layout, install, &[], ctl, signer)
}

/// [`manage`], where uninstall also owns the exact registration of each `also` binary.
///
/// Why: `B uninstall-service` builds its layout for a substituted S, yet the
/// registration may name B; uninstall records nothing, so matching both is
/// safe (#11927 review).
/// What: on uninstall, a plist or launcher byte-identical to what `also[i]`
/// would write counts as [`Presence::Previous`] and is removed. Install
/// ignores `also`, so it never takes over another binary's registration.
/// Test: `uninstall_from_a_build_binary_matches_its_own_registration`,
/// `uninstall_service_takes_over_a_registration_whose_binary_is_gone` in `tests/install.rs`.
pub fn manage_with(
    layout: &Layout,
    install: bool,
    also: &[PathBuf],
    ctl: &dyn Launchctl,
    signer: &Signer<'_>,
) -> Result<(), LaunchdError> {
    let also = if install { &[][..] } else { also };
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
    // #11927 review: uninstall also owns the exact registration of each `also` binary.
    let also_plists = also
        .iter()
        .map(|o| launch_configuration(o, state))
        .collect::<io::Result<Vec<_>>>()?;
    let also_hosts: Vec<_> = also
        .iter()
        .map(|o| install_host::registration_files(state, &manifests, o))
        .collect();
    let present = installed(&plist_path, &configuration, 0o600, &move |t| {
        // #11927: also our plist for a binary that is gone.
        python_plist(t, &st) || stale_plist(t, &st) || also_plists.iter().any(|p| p == t)
    })?;
    let loaded = ctl.run(&["print", &target])?;
    let marker = format!("path = {}\n", plist_path.display());
    if loaded.code == 0 && (present == Presence::Absent || !loaded.stdout.contains(&marker)) {
        return Err(io::Error::other(format!("A different launchd job owns {LABEL}")).into());
    }
    let mut owned = Vec::new();
    for (i, r) in registrations.iter().enumerate() {
        let legacy = legacy_for(r, &layout.exe);
        let other = |t: &str| also_hosts.iter().any(|h| h[i].content == t);
        let p = installed(&r.path, &r.content, r.mode, &|t| legacy(t) || other(t))?;
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
            if *p == Presence::Previous {
                install_host::replace_owned(&r.path, &r.content, r.mode)?;
            }
        }
        install_host::install(state, &manifests, &layout.exe)?;
        if present == Presence::Previous {
            install_host::replace_owned(&plist_path, &configuration, 0o600)?;
        }
        install_host::write_owned(&plist_path, &configuration, 0o600)?;
        // #11440: a loaded Python job keeps running its old plist until booted out.
        if loaded.code == 0 && present == Presence::Previous {
            checked(ctl, &["bootout", &target])?;
        }
        if loaded.code != 0 || present == Presence::Previous {
            checked(ctl, &["bootstrap", &domain, &plist_path.to_string_lossy()])?;
        } else {
            // #11522: a loaded job keeps its old process; restart it so an upgrade
            // serves the new binary and asset stamp. A failure is an error (exit 1),
            // never a successful install.
            checked(ctl, &["kickstart", "-k", &target])?;
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
