//! How `tm launch` and `tm connect` hand a running session over: attach this
//! terminal, or report the session and return (#8406).
//!
//! Why: `tm launch` always ended in `tmux_attach`, which inside tmux switches
//! the caller's own client to the new session. A supervisor in its own pane
//! could not start a watched project without losing that pane.
//! What: [`Handoff::Attach`] keeps the full-screen banner and the attach;
//! [`Handoff::Detach`] prints neither, and writes [`detached_line`] — the tmux
//! session name and its pane id — to stdout instead.
//! Test: `detach_reports_the_session_and_never_attaches`,
//! `attach_attaches_and_prints_no_line`,
//! `detach_fails_when_the_pane_id_cannot_be_read`,
//! `pane_id_query_targets_the_session_exactly`, `cli_parses_launch_with_detach`.

use std::io::Write;
use std::path::Path;

use crate::formatters::banner::{print_launch_banner, print_launch_banner_reconnecting};

/// What a launch does with its session once `claude` runs in it (#8406).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// Print the banner, then attach (or switch) this terminal to the session.
    Attach,
    /// Print `session=<name> pane=<pane id>` and return; the terminal is left alone.
    Detach,
}

impl Handoff {
    /// The handoff `tm launch --detach` asks for.
    pub(crate) fn from_detach(detach: bool) -> Self {
        if detach { Self::Detach } else { Self::Attach }
    }

    /// The new-session banner; a detached launch prints none, since the banner
    /// clears the screen of the pane it runs in.
    pub(crate) fn announce(
        self,
        workdir: &str,
        name: &str,
        prompt: Option<&Path>,
        managed: Option<&Path>,
    ) {
        if self == Self::Attach {
            print_launch_banner(workdir, name, prompt, managed);
        }
    }

    /// [`Self::announce`] for a session that was already running.
    pub(crate) fn announce_reconnect(self, workdir: &str, name: &str) {
        if self == Self::Attach {
            print_launch_banner_reconnecting(workdir, name);
        }
    }

    /// Hand the running tmux session `name` over.
    ///
    /// Why: the one point where `tm launch` either takes the terminal or
    /// returns; a caller that disarms its session guard after this call keeps
    /// the session in both cases.
    /// What: `Attach` runs [`crate::commands::tmux_attach::tmux_attach`].
    /// `Detach` reads the session's pane id and prints [`detached_line`]; a pane
    /// id it cannot read is an error, because the caller asked for it.
    /// Test: `detach_reports_the_session_and_never_attaches`,
    /// `attach_attaches_and_prints_no_line`,
    /// `detach_fails_when_the_pane_id_cannot_be_read`.
    pub(crate) fn hand_over(self, name: &str) -> anyhow::Result<()> {
        self.hand_over_with(
            name,
            |n| crate::commands::tmux_attach::tmux_attach(n).map(drop),
            tmux_stdout,
            &mut std::io::stdout(),
        )
    }

    /// [`Self::hand_over`] over injected attach, tmux and output seams.
    fn hand_over_with(
        self,
        name: &str,
        attach: impl FnOnce(&str) -> anyhow::Result<()>,
        tmux: impl FnOnce(&[String]) -> Option<String>,
        out: &mut impl Write,
    ) -> anyhow::Result<()> {
        match self {
            Self::Attach => attach(name),
            Self::Detach => {
                let pane = session_pane_id_with(name, tmux).ok_or_else(|| {
                    anyhow::anyhow!(
                        "tmux session {name} is running, but its pane id could not be read \
                         (try `tmux list-panes -t {name}`)"
                    )
                })?;
                writeln!(out, "{}", detached_line(name, &pane))?;
                Ok(())
            }
        }
    }
}

/// The one stdout line a detached launch prints: `session=<name> pane=<id>`.
pub(crate) fn detached_line(name: &str, pane: &str) -> String {
    format!("session={name} pane={pane}")
}

/// The pane id (`%N`) of the active pane of tmux session `name`, read through
/// `tmux`, which runs one tmux argv and answers its stdout.
///
/// `display-message` targets `=<name>:` so a name that is a prefix of another
/// session never answers for that session (#8443); anything that is not a
/// `%N` id answers `None`.
fn session_pane_id_with(
    name: &str,
    tmux: impl FnOnce(&[String]) -> Option<String>,
) -> Option<String> {
    let target = trusty_mpm::core::tmux::TmuxTarget::try_session(name).ok()?;
    let argv = trusty_mpm::core::tmux::display_message_argv(Some(&target), "#{pane_id}");
    let pane = tmux(&argv)?.trim().to_string();
    (pane.starts_with('%') && trusty_mpm::core::tmux::is_immutable_id(&pane)).then_some(pane)
}

/// Run one tmux argv; its stdout on a zero exit, `None` otherwise.
fn tmux_stdout(argv: &[String]) -> Option<String> {
    let output = trusty_mpm::core::tmux::run_tmux_argv(argv).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_reports_the_session_and_never_attaches() {
        let mut out = Vec::new();
        Handoff::Detach
            .hand_over_with(
                "tm-proj",
                |_| panic!("a detached launch must never attach"),
                |_| Some("%42\n".to_string()),
                &mut out,
            )
            .expect("detached handoff");
        assert_eq!(String::from_utf8_lossy(&out), "session=tm-proj pane=%42\n");
    }

    #[test]
    fn attach_attaches_and_prints_no_line() {
        let mut out = Vec::new();
        let mut attached = None;
        Handoff::Attach
            .hand_over_with(
                "tm-proj",
                |n| {
                    attached = Some(n.to_string());
                    Ok(())
                },
                |_| panic!("an attached launch reads no pane id"),
                &mut out,
            )
            .expect("attached handoff");
        assert_eq!(attached.as_deref(), Some("tm-proj"));
        assert!(out.is_empty());
    }

    #[test]
    fn detach_fails_when_the_pane_id_cannot_be_read() {
        for answer in [None, Some(String::new()), Some("tm-proj".to_string())] {
            let mut out = Vec::new();
            let err = Handoff::Detach
                .hand_over_with("tm-proj", |_| Ok(()), |_| answer.clone(), &mut out)
                .expect_err("no pane id, no report");
            assert!(err.to_string().contains("pane id"), "{err}");
            assert!(out.is_empty(), "{answer:?}");
        }
    }

    #[test]
    fn pane_id_query_targets_the_session_exactly() {
        let mut seen = Vec::new();
        let pane = session_pane_id_with("tm-proj", |argv| {
            seen = argv.to_vec();
            Some("%7".to_string())
        });
        assert_eq!(pane.as_deref(), Some("%7"));
        assert_eq!(
            seen,
            ["display-message", "-t", "=tm-proj:", "-p", "#{pane_id}"]
        );
        assert_eq!(session_pane_id_with("", |_| Some("%7".into())), None);
    }

    #[test]
    fn from_detach_maps_the_flag() {
        assert_eq!(Handoff::from_detach(true), Handoff::Detach);
        assert_eq!(Handoff::from_detach(false), Handoff::Attach);
    }
}
