//! The live [`PaneProbe`] of the Architect pane floor (#8902).
//!
//! Why: the floor binds the Architect's pane to the existing launch record
//! (#8878 ruling A), not to a registry of its own.
//! What: [`LivePanes`] reads the live Architect lineage from
//! `~/.trusty-mpm/architect-launch/` ([`live_architect_lineage`]), lists the
//! panes of the server a tmux invocation selects, and marks a pane as the
//! Architect's when its `#{pane_pid}` is in that lineage or its session is
//! the launch session `tm-architect`. Read only when a deny-set tmux command
//! is in the call.
//! Test: `a_pane_listing_marks_the_architect_by_lineage_and_session`; end to
//! end in `tests/tm_hook_pm_guard_architect_pane_8902.rs`.

use std::cell::OnceCell;
use std::path::PathBuf;

use trusty_mpm::core::architect_launch::live_architect_lineage;

use super::architect_pane::{Pane, PaneProbe};
use crate::commands::fleet::launch::ARCHITECT_SESSION;

/// The `list-panes -F` format [`parse_panes`] reads, tab-separated.
const PANE_FORMAT: &str =
    "#{pane_id}\t#{window_id}\t#{session_id}\t#{pane_pid}\t#{pane_marked}\t#{session_name}";

/// The probe over this process's home, tmux and process table.
pub(crate) struct LivePanes {
    root: Option<PathBuf>,
    lineage: OnceCell<Result<Vec<u32>, String>>,
}

impl LivePanes {
    /// The probe over `~/.trusty-mpm`, the root the launch records live in.
    pub(crate) fn ambient() -> Self {
        Self {
            root: dirs::home_dir()
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".trusty-mpm")),
            lineage: OnceCell::new(),
        }
    }

    fn lineage(&self) -> &Result<Vec<u32>, String> {
        self.lineage.get_or_init(|| match &self.root {
            Some(root) => live_architect_lineage(root),
            None => Err("no home directory".into()),
        })
    }
}

impl PaneProbe for LivePanes {
    fn architect_live(&self) -> Result<bool, String> {
        self.lineage()
            .as_ref()
            .map(|l| !l.is_empty())
            .map_err(Clone::clone)
    }

    fn panes(&self, server: &[String]) -> Result<Vec<Pane>, String> {
        let mut argv = server.to_vec();
        argv.extend(["list-panes", "-a", "-F", PANE_FORMAT].map(str::to_owned));
        let run = trusty_mpm::core::tmux::run_tmux_argv(&argv);
        let (stdout, stderr) = match &run {
            Ok(out) => (
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            ),
            Err(_) => Default::default(),
        };
        let listed = match &run {
            Ok(out) => Listed::Ran {
                ok: out.status.success(),
                stdout: &stdout,
                stderr: &stderr,
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Listed::NotFound,
            Err(err) => Listed::Failed(err.to_string()),
        };
        classify_listing(listed, self.lineage())
    }

    fn current_pane(&self) -> Option<String> {
        std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())
    }
}

/// What one `tmux list-panes` run gave.
#[derive(Debug)]
pub(super) enum Listed<'a> {
    /// No tmux binary: the command under judgement cannot run either.
    NotFound,
    /// tmux could not be started.
    Failed(String),
    /// tmux ran: whether it exited 0, and its output.
    Ran {
        ok: bool,
        stdout: &'a str,
        stderr: &'a str,
    },
}

/// The pane list one `list-panes` run gives, the Architect's panes marked.
///
/// What: no tmux binary, and a failure naming no running server or a socket
/// tmux cannot connect to, are an empty list: the command cannot reach that
/// server either. Any other failure is `Err`. An unreadable `lineage` still
/// marks the panes of the launch session.
/// Test: `a_pane_listing_run_is_classified`.
pub(super) fn classify_listing(
    listed: Listed<'_>,
    lineage: &Result<Vec<u32>, String>,
) -> Result<Vec<Pane>, String> {
    match listed {
        Listed::NotFound => Ok(Vec::new()),
        Listed::Failed(err) => Err(err),
        Listed::Ran {
            ok: false, stderr, ..
        } => {
            let err = stderr.trim();
            if ["no server running", "error connecting to"]
                .iter()
                .any(|m| err.contains(m))
            {
                return Ok(Vec::new());
            }
            Err(err.to_owned())
        }
        Listed::Ran {
            ok: true, stdout, ..
        } => parse_panes(stdout, lineage.as_deref().unwrap_or_default()),
    }
}

/// Parse [`PANE_FORMAT`] lines, marking the Architect's panes.
///
/// Test: `a_pane_listing_marks_the_architect_by_lineage_and_session`.
pub(super) fn parse_panes(text: &str, lineage: &[u32]) -> Result<Vec<Pane>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.splitn(6, '\t').collect();
            let [pane, window, session, pid, marked, name] = f[..] else {
                return Err(format!("unreadable tmux pane line {line:?}"));
            };
            let pid: u32 = pid
                .parse()
                .map_err(|_| format!("unreadable pane pid in {line:?}"))?;
            Ok(Pane {
                pane: pane.to_owned(),
                window: window.to_owned(),
                session: session.to_owned(),
                name: name.to_owned(),
                architect: name == ARCHITECT_SESSION || lineage.contains(&pid),
                marked: marked == "1",
            })
        })
        .collect()
}
