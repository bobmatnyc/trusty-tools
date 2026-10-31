//! The Architect dashboard's per-user LaunchAgent (#10571 P1b).
//!
//! Why: a dashboard started by hand dies with the login session or a crash, so
//! a fresh install must bring it back on its own. A host that already runs a
//! dashboard must never get a second one on the same port or the same store.
//! What: [`steps`] runs after `tm fleet init` seeds the scripts. It reads
//! what listens on the dashboard port and classifies it ([`classify`]):
//! nothing, this Architect's own agent, a known managed dashboard (its
//! `scripts/dashboard-serve.py` is a version tm shipped and its cwd is the
//! Architect directory), or anything else. Nothing listening: write the plist
//! (`RunAtLoad`, `KeepAlive` restart on a crash), load it, check that it
//! answers, and roll back if it does not. A known dashboard is moved under the
//! agent only with `--dashboard-takeover` ([`takeover`]); anything else is
//! refused and left alone. Every launchd, process and HTTP call goes through
//! [`Host`], so the decisions are tested with a fake on any platform.
//! Test: `dashboard_agent_tests.rs`.

use std::path::{Path, PathBuf};

use super::Step;
use super::seed::refresh;

/// The dashboard script, below the Architect directory.
pub(crate) const SCRIPT: &str = "scripts/dashboard-serve.py";
/// The default port when `local/architect.json` names none.
pub(crate) const DEFAULT_PORT: u16 = 7890;

/// The agent's launchd label: a sub-unit of the mpm daemon's label.
pub(crate) fn label() -> String {
    use trusty_common::launchd_labels::{canonical_label, sub_label};
    sub_label(&canonical_label("trusty-mpm"), "architect-dashboard")
}

/// The plist path for `label` under `home`.
pub(crate) fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{}.plist", label()))
}

/// Where the agent writes the dashboard's stdout and stderr.
pub(crate) fn log_dir(dir: &Path) -> PathBuf {
    dir.join("inbox").join("dashboard-logs")
}

/// One process listening on the dashboard port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listener {
    /// Its process id.
    pub(crate) pid: u32,
    /// Its command line, one argument per element.
    pub(crate) argv: Vec<String>,
    /// Its working directory, when it could be read.
    pub(crate) cwd: Option<PathBuf>,
}

/// The side effects [`steps`] needs; [`RealHost`] in production, a fake in tests.
pub(crate) trait Host {
    /// The process listening on `127.0.0.1:port`: `Ok(None)` when the probe ran
    /// and found nothing, `Err` when it could not tell (a missing or failing
    /// `lsof` or `ps`, or more than one process) - never read as a free port.
    fn listener(&self, port: u16) -> Result<Option<Listener>, String>;
    /// Whether launchd has `label` loaded.
    fn agent_loaded(&self, label: &str) -> bool;
    /// The plist XML for the agent ([`render_plist`] on macOS).
    fn render(&self, dir: &Path, port: u16) -> anyhow::Result<String>;
    /// Load the agent from `plist`.
    fn bootstrap(&self, plist: &Path) -> anyhow::Result<()>;
    /// Unload `label`; not loaded is not an error.
    fn bootout(&self, label: &str) -> anyhow::Result<()>;
    /// Stop `pid` with SIGTERM; true once it has exited.
    fn terminate(&self, pid: u32) -> bool;
    /// Start the known dashboard again, detached: `python3 <dir>/scripts/dashboard-serve.py
    /// --port <port>` with cwd `dir`, each argument passed as is (rollback).
    fn respawn(&self, dir: &Path, port: u16) -> anyhow::Result<()>;
    /// Whether a dashboard answers on `port` (waits a few seconds).
    fn serving(&self, port: u16) -> bool;
}

/// What holds the dashboard port.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Existing {
    /// Nothing listens.
    Free,
    /// This Architect's LaunchAgent is loaded.
    Ours,
    /// A dashboard tm shipped, running from the Architect directory.
    KnownManaged(Listener),
    /// Something else; the string says why, in plain words.
    Unknown(Listener, String),
    /// The port could not be checked; the string says why.
    Unchecked(String),
}

/// Classify what holds `port` for the Architect at `dir`.
///
/// Why: only a dashboard that is provably ours may be stopped and replaced.
/// What: `Ours` when the agent is loaded. Else no listener is `Free`. A
/// listener is `KnownManaged` when its argv names `dir`'s
/// `scripts/dashboard-serve.py`, its cwd is `dir`, and that file's bytes are
/// the shipped copy or a version in `seed_history.tsv` (the refresh path's
/// classification); else `Unknown` with the first reason that failed.
/// Test: `a_known_dashboard_is_classified_known`, `an_unknown_listener_is_refused`.
pub(crate) fn classify(dir: &Path, port: u16, host: &dyn Host, shipped: &str) -> Existing {
    if host.agent_loaded(&label()) {
        return Existing::Ours;
    }
    let l = match host.listener(port) {
        Ok(Some(l)) => l,
        Ok(None) => return Existing::Free,
        Err(why) => return Existing::Unchecked(why),
    };
    match known_reason(dir, &l, shipped) {
        None => Existing::KnownManaged(l),
        Some(why) => Existing::Unknown(l, why),
    }
}

/// `None` when `l` is this Architect's managed dashboard, else why not.
fn known_reason(dir: &Path, l: &Listener, shipped: &str) -> Option<String> {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = real(dir);
    let Some(arg) = l.argv.iter().find(|a| a.ends_with("dashboard-serve.py")) else {
        return Some("it is not a dashboard-serve.py process".to_owned());
    };
    let Some(cwd) = l.cwd.as_deref() else {
        return Some("its working folder could not be read".to_owned());
    };
    if real(cwd) != dir {
        return Some("it runs from another folder".to_owned());
    }
    let script = real(&cwd.join(arg));
    if script != real(&dir.join(SCRIPT)) {
        return Some("it runs a dashboard script from another folder".to_owned());
    }
    match std::fs::read(&script) {
        Ok(bytes) if bytes == shipped.as_bytes() || refresh::shipped_before(SCRIPT, &bytes) => None,
        Ok(_) => Some("its dashboard script was changed by hand".to_owned()),
        Err(_) => Some("its dashboard script could not be read".to_owned()),
    }
}

/// The dashboard port from `dir`'s `local/architect.json`, else [`DEFAULT_PORT`].
pub(crate) fn port_of(dir: &Path) -> u16 {
    std::fs::read_to_string(dir.join("local/architect.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("dashboard_port")?.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
        .unwrap_or(DEFAULT_PORT)
}

/// [`steps`] for the real host, after two refusals that keep a scratch
/// environment out of the user's launchd (#10571).
///
/// Why: launchd belongs to the login user. A run under another `HOME` (a test,
/// a sandbox) must never load an agent there, and off macOS there is no launchd.
/// What: off macOS, one plain `Skipped` line; on macOS with `home` not the
/// password-database home of this uid, a `Skipped` line naming both; else
/// [`steps`] with [`RealHost`].
/// Test: `fleet_init_launches_the_architect_and_status_is_complete` (a temp HOME: skipped).
pub(crate) fn login_steps(
    dir: &Path,
    home: &Path,
    launch: bool,
    takeover_ok: bool,
    shipped: &str,
) -> Vec<Step> {
    if !launch {
        return steps(dir, home, false, takeover_ok, &RealHost, shipped);
    }
    if !cfg!(target_os = "macos") {
        return vec![Step::Skipped(
            "dashboard login start: needs macOS (launchd); start the dashboard with \
             `python3 scripts/dashboard-serve.py`"
                .to_owned(),
        )];
    }
    let real = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .ok()
        .flatten()
        .map(|u| u.dir);
    let same = real
        .as_deref()
        .and_then(|r| std::fs::canonicalize(r).ok())
        .zip(std::fs::canonicalize(home).ok())
        .is_some_and(|(a, b)| a == b);
    if !same {
        return vec![Step::Skipped(format!(
            "dashboard login start: skipped, because {} is not this user's home folder; a \
             login start belongs to the real login",
            home.display()
        ))];
    }
    steps(dir, home, launch, takeover_ok, &RealHost, shipped)
}

/// The report lines for the dashboard's login start.
///
/// Why/What: see the module doc. `launch` false (`--no-launch`) changes
/// nothing; `takeover` allows [`takeover`] of a known managed dashboard.
/// Test: `a_free_port_installs_and_loads_the_agent`,
/// `a_restart_keeps_edited_state_and_starts_no_duplicate`.
pub(crate) fn steps(
    dir: &Path,
    home: &Path,
    launch: bool,
    takeover_ok: bool,
    host: &dyn Host,
    shipped: &str,
) -> Vec<Step> {
    if !launch {
        return vec![Step::Skipped(
            "dashboard login start (--no-launch): nothing was changed".to_owned(),
        )];
    }
    let port = port_of(dir);
    let step = match classify(dir, port, host, shipped) {
        Existing::Ours if host.serving(port) => Step::Unchanged(format!(
            "dashboard: starts at login and is running on port {port}"
        )),
        Existing::Ours => Step::Skipped(format!(
            "dashboard: set to start at login, but it is not answering on port {port}; \
             its logs are in {}",
            log_dir(dir).display()
        )),
        Existing::Free => install(dir, home, port, host),
        Existing::KnownManaged(l) if takeover_ok => takeover(dir, home, port, host, &l),
        Existing::KnownManaged(l) => Step::Skipped(format!(
            "dashboard: this Architect's dashboard is already running (pid {}) but will not \
             restart after a login or a crash. To fix that, run `tm fleet init --dir {} \
             --dashboard-takeover`. Nothing was changed.",
            l.pid,
            dir.display()
        )),
        Existing::Unchecked(why) => Step::Skipped(format!(
            "dashboard: could not check what uses port {port} ({why}), so the login start was \
             not set up and nothing was stopped"
        )),
        Existing::Unknown(l, why) => Step::Skipped(format!(
            "dashboard: another program is using port {port} (pid {}), and {why}, so it is not \
             this Architect's dashboard. The login start was not set up and nothing was stopped.",
            l.pid
        )),
    };
    vec![step]
}

/// Write and load the agent, check it answers, and undo both when it does not.
fn install(dir: &Path, home: &Path, port: u16, host: &dyn Host) -> Step {
    match load_and_verify(dir, home, port, host) {
        Ok(note) => Step::Changed(format!(
            "dashboard: set to start at login and after a crash, running on port {port} \
             (launchd {}){note}",
            label()
        )),
        Err(why) => Step::Failed(format!(
            "dashboard: could not set it to start at login ({why}); the change was undone"
        )),
    }
}

/// Write the plist, bootstrap it and wait for an answer, keeping any plist
/// already there.
///
/// Why: the critic's #10571 data-loss finding: a failed attempt must leave the
/// plist exactly as it found it, and a success must not lose the old one.
/// What: render, then create the two directories (no plist touched yet); read
/// the existing plist; write the new one; bootstrap; verify. A failure after
/// the write boots the agent out and puts back the old bytes, or removes the
/// plist only when this call created it. On success an existing plist is kept
/// as `<plist>.bak` and the returned note says so.
/// Test: `a_pre_existing_plist_survives_every_failure`,
/// `a_replaced_plist_is_kept_as_bak`.
fn load_and_verify(dir: &Path, home: &Path, port: u16, host: &dyn Host) -> Result<String, String> {
    let plist = plist_path(home);
    let xml = host.render(dir, port).map_err(|e| e.to_string())?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(log_dir(dir)).map_err(|e| e.to_string())?;
    let original = match std::fs::read(&plist) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "could not read the existing {}: {e}",
                plist.display()
            ));
        }
    };
    trusty_common::atomic_file::write_atomic(&plist, xml.as_bytes()).map_err(|e| e.to_string())?;
    let started = host
        .bootstrap(&plist)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            host.serving(port)
                .then_some(())
                .ok_or_else(|| "it did not answer after starting".to_owned())
        });
    if let Err(why) = started {
        let _ = host.bootout(&label());
        let undo = match &original {
            Some(bytes) => {
                trusty_common::atomic_file::write_atomic(&plist, bytes).map_err(|e| e.to_string())
            }
            None => std::fs::remove_file(&plist).map_err(|e| e.to_string()),
        };
        return Err(match undo {
            Ok(()) => why,
            Err(e) => format!("{why}; the plist could not be put back: {e}"),
        });
    }
    Ok(match original {
        Some(bytes) if bytes != xml.as_bytes() => {
            let bak = plist.with_extension("plist.bak");
            match trusty_common::atomic_file::write_atomic(&bak, &bytes) {
                Ok(()) => format!(
                    "; the earlier plist was replaced and kept as {}",
                    bak.display()
                ),
                Err(e) => {
                    format!("; the earlier plist was replaced, and its copy could not be kept: {e}")
                }
            }
        }
        _ => String::new(),
    })
}

/// Move a known managed dashboard under the agent, in checkpoints.
///
/// Why: an existing host may already run a known dashboard on the port; replacing it
/// must never leave two, or none.
/// What: (0) re-read the listener and stop unless it is still `l` (same pid
/// and argv); (1) stop it with SIGTERM and wait; (2) confirm the port is free;
/// (3) [`load_and_verify`], which restores the plist on its own failure;
/// (4) on a failure in 2 or 3, start the known dashboard command again and say
/// whether it answers. Each failure names the step.
/// Test: `a_known_dashboard_is_taken_over`,
/// `a_failed_takeover_puts_the_old_dashboard_back`.
pub(crate) fn takeover(dir: &Path, home: &Path, port: u16, host: &dyn Host, l: &Listener) -> Step {
    // #10571: the process may have changed since it was classified; check again first.
    match host.listener(port) {
        Ok(Some(now)) if now.pid == l.pid && now.argv == l.argv => {}
        _ => {
            return Step::Failed(format!(
                "dashboard: what runs on port {port} changed while preparing the move (pid {}), \
                 so nothing was stopped; run tm fleet init again",
                l.pid
            ));
        }
    }
    if !host.terminate(l.pid) {
        return Step::Failed(format!(
            "dashboard: sent the running dashboard (pid {}) a stop signal, but it did not exit \
             within 10 seconds; it may still be stopping. The login start was not set up.",
            l.pid
        ));
    }
    let failure = match host.listener(port) {
        Ok(Some(other)) => format!(
            "port {port} was still in use (pid {}) after stopping it",
            other.pid
        ),
        Err(why) => format!("port {port} could not be checked after stopping it ({why})"),
        Ok(None) => match load_and_verify(dir, home, port, host) {
            Ok(note) => {
                return Step::Changed(format!(
                    "dashboard: moved under login start: stopped pid {} and launchd now runs it \
                     on port {port}{note}",
                    l.pid
                ));
            }
            Err(why) => why,
        },
    };
    let back = host.respawn(dir, port).is_ok() && host.serving(port);
    Step::Failed(format!(
        "dashboard: the move to login start failed ({failure}). The previous dashboard {}.",
        if back {
            "was started again and is answering"
        } else {
            "could not be started again; start it with `python3 scripts/dashboard-serve.py`"
        }
    ))
}

/// The agent's plist, from the shared trusty-common renderer (macOS only).
///
/// Why: every trusty-* LaunchAgent is generated by one audited renderer.
/// What: `python3 <dir>/scripts/dashboard-serve.py --port <port>`, cwd `dir`,
/// `RunAtLoad`, `KeepAlive` that restarts on a non-zero exit (a refused second
/// copy exits 0, so it never loops), logs under [`log_dir`].
/// Test: `the_plist_has_the_login_and_restart_keys` (macOS).
#[cfg(target_os = "macos")]
pub(crate) fn render_plist(dir: &Path, port: u16) -> anyhow::Result<String> {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};
    let python = trusty_common::bin_resolve::resolve_binary("python3")
        .unwrap_or_else(|| PathBuf::from("/usr/bin/python3"));
    LaunchdConfig {
        label: label(),
        exe_path: python,
        args: vec![
            dir.join(SCRIPT).display().to_string(),
            "--port".to_owned(),
            port.to_string(),
        ],
        log_dir: log_dir(dir),
        keep_alive: KeepAlive::OnSuccess,
        throttle_interval: 10,
        env_vars: Vec::new(),
        fd_limit: None,
        working_directory: Some(dir.display().to_string()),
    }
    .with_daemon_path()
    .render_plist()
}

/// No launchd off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) fn render_plist(_dir: &Path, _port: u16) -> anyhow::Result<String> {
    anyhow::bail!("starting the dashboard at login needs macOS (launchd)")
}

#[path = "dashboard_agent_host.rs"]
mod host;
pub(crate) use host::RealHost;
