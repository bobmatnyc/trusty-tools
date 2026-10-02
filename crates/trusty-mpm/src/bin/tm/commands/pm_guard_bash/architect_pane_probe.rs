//! The live [`PaneProbe`] of the Architect pane floor (#8902).
//!
//! Why: the floor binds the Architect's pane to the existing launch record
//! (#8878 ruling A), not to a registry of its own.
//! What: [`LivePanes`] reads the live Architect lineage from
//! `~/.trusty-mpm/architect-launch/` ([`live_architect_lineage`]), lists the
//! panes of the server a tmux invocation selects, and marks a pane as the
//! Architect's when its `#{pane_pid}` is in that lineage or its session is
//! `tm-architect`. When the lineage does not read, the sessions recorded in
//! the `*.architect-session` sidecars (`tm fleet init --session`) are marked
//! too; when those do not read either, the listing is `Err` and the command
//! is denied. Read only when a deny-set tmux command is in the call.
//! #9001 critic r1: every listing of one probe shares [`LISTING_BUDGET`]; a
//! tmux that does not answer in time is `Err`, which denies.
//! Test: `a_pane_listing_marks_the_architect_by_lineage_and_session`,
//! `a_pane_listing_run_is_classified`,
//! `a_stopped_tmux_server_times_out_the_listing`; end to end in
//! `tests/tm_hook_pm_guard_architect_pane_8902.rs`.

use std::cell::OnceCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use trusty_mpm::core::architect_launch::live_architect_lineage;
use trusty_mpm::core::architect_session::recorded_session_names;

use super::architect_pane::{Pane, PaneProbe};
use super::tmux_exact_target::{OBJECT_FORMAT, TmuxObject, classify_objects};
use crate::commands::fleet::launch::ARCHITECT_SESSION;

/// The `list-panes -F` format [`parse_panes`] reads, tab-separated.
const PANE_FORMAT: &str =
    "#{pane_id}\t#{window_id}\t#{session_id}\t#{pane_pid}\t#{pane_marked}\t#{session_name}";

/// The probe over this process's home, tmux and process table.
pub(crate) struct LivePanes {
    root: Option<PathBuf>,
    lineage: OnceCell<Result<Vec<u32>, String>>,
    marks: OnceCell<Result<ArchitectMarks, String>>,
    /// When [`LISTING_BUDGET`] runs out, set by the first listing.
    deadline: OnceCell<Instant>,
}

/// How long one probe may spend in every tmux listing it runs (#9001).
///
/// Why: the hook is killed at 5 s, and a stopped server never answers; the
/// #8902 and #9001 floors may each list the same server.
pub(super) const LISTING_BUDGET: Duration = Duration::from_secs(2);

/// What marks a pane as the Architect's, beside the `tm-architect` name.
#[derive(Debug, Default)]
pub(super) struct ArchitectMarks {
    /// The live Architect lineage, by PID.
    pub(super) lineage: Vec<u32>,
    /// Recorded Architect session names, read only when the lineage is not.
    pub(super) sessions: Vec<String>,
}

/// The marks for one probe: the lineage when it reads, else every recorded
/// session name.
///
/// Why: #8878 R1 round 2 — with the lineage unreadable, only `tm-architect`
/// was marked, so a PM could drive an Architect run under `--session`.
/// What: `Ok(lineage)` marks by PID, and `sidecars` is not called. `Err`
/// marks the names `sidecars` gives; when it fails too, `Err`, which denies
/// every deny-set command.
/// Test: `a_pane_listing_run_is_classified`.
pub(super) fn architect_marks(
    lineage: &Result<Vec<u32>, String>,
    sidecars: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<ArchitectMarks, String> {
    match lineage {
        Ok(lineage) => Ok(ArchitectMarks {
            lineage: lineage.clone(),
            sessions: Vec::new(),
        }),
        // #8878: an unreadable lineage falls back to the recorded names.
        Err(err) => sidecars()
            .map(|sessions| ArchitectMarks {
                lineage: Vec::new(),
                sessions,
            })
            .map_err(|why| format!("the Architect launch records do not read ({err}; {why})")),
    }
}

impl LivePanes {
    /// The probe over `~/.trusty-mpm`, the root the launch records live in.
    pub(crate) fn ambient() -> Self {
        Self {
            root: dirs::home_dir()
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".trusty-mpm")),
            lineage: OnceCell::new(),
            marks: OnceCell::new(),
            deadline: OnceCell::new(),
        }
    }

    /// What is left of [`LISTING_BUDGET`]; the first call starts it.
    fn budget(&self) -> Duration {
        let end = *self
            .deadline
            .get_or_init(|| Instant::now() + LISTING_BUDGET);
        end.saturating_duration_since(Instant::now())
    }

    /// One listing on `server`, bounded by what is left of the budget.
    fn list<T>(
        &self,
        server: &[String],
        format: &str,
        classify: impl FnOnce(Listed<'_>) -> T,
    ) -> T {
        let budget = self.budget();
        list_panes(server, format, classify, |argv| {
            if budget.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the tmux listing budget is spent",
                ));
            }
            trusty_mpm::core::tmux::run_tmux_argv_bounded(argv, budget)
        })
    }

    fn lineage(&self) -> &Result<Vec<u32>, String> {
        self.lineage.get_or_init(|| match &self.root {
            Some(root) => live_architect_lineage(root),
            None => Err("no home directory".into()),
        })
    }

    fn marks(&self) -> &Result<ArchitectMarks, String> {
        self.marks.get_or_init(|| {
            architect_marks(self.lineage(), || match &self.root {
                Some(root) => recorded_session_names(root),
                None => Err("no home directory".into()),
            })
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
        self.list(server, PANE_FORMAT, |listed| {
            classify_listing(listed, self.marks())
        })
    }

    fn current_pane(&self) -> Option<String> {
        std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())
    }

    fn objects(&self, server: &[String]) -> Result<Vec<TmuxObject>, String> {
        self.list(server, OBJECT_FORMAT, classify_objects)
    }
}

/// Run `tmux <server> list-panes -a -F <format>` through `run` and hand what
/// it gave to `classify`; a run that timed out is [`Listed::Failed`].
pub(super) fn list_panes<T>(
    server: &[String],
    format: &str,
    classify: impl FnOnce(Listed<'_>) -> T,
    run: impl FnOnce(&[String]) -> std::io::Result<std::process::Output>,
) -> T {
    let mut argv = server.to_vec();
    argv.extend(["list-panes", "-a", "-F", format].map(str::to_owned));
    let run = run(&argv);
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
    classify(listed)
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
/// server either. Any other failure is `Err`, and so is a listing when
/// `marks` is `Err` ([`architect_marks`]): no pane can be proved not the
/// Architect's.
/// Test: `a_pane_listing_run_is_classified`.
pub(super) fn classify_listing(
    listed: Listed<'_>,
    marks: &Result<ArchitectMarks, String>,
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
        } => {
            // #8878: no marks means no pane is proved the Architect's or not.
            let marks = marks.as_ref().map_err(Clone::clone)?;
            parse_panes(stdout, &marks.lineage, &marks.sessions)
        }
    }
}

/// Parse [`PANE_FORMAT`] lines, marking a pane in `tm-architect` or in one of
/// `sessions`, or whose pid is in `lineage`, as the Architect's.
///
/// Test: `a_pane_listing_marks_the_architect_by_lineage_and_session`.
pub(super) fn parse_panes(
    text: &str,
    lineage: &[u32],
    sessions: &[String],
) -> Result<Vec<Pane>, String> {
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
                architect: name == ARCHITECT_SESSION
                    || sessions.iter().any(|s| s == name)
                    || lineage.contains(&pid),
                marked: marked == "1",
            })
        })
        .collect()
}
