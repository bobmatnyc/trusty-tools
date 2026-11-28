//! `questions_rc.py`: Remote Control on the questions session follows the
//! open interactive questions (#12029 P2f).
//!
//! Why: the poller runs the script each cycle and the dashboard kicks it when
//! the last interactive question is answered; the owner answers from the
//! Claude app, which needs Remote Control on only while a question waits.
//! What: [`run`] is the script's `main`: count the open ids ([`open_ids`]),
//! read the session (`has-session`, then [`rc_from_capture`] on the pane),
//! [`decide`], run the action's script, re-read the pane until it shows the
//! wanted state ([`settle`]), print one line and write the state file
//! ([`write_state`], temp file then rename). `--dry-run` runs no action and
//! writes nothing. The session is never killed.
//! Test: `questions_rc_recordings_replay_clean_against_the_port`,
//! `rc_fails_closed_on_a_failed_tmux_call_or_write`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use super::{QuestionsError, TmuxCli, run_command};
use crate::clock::Clock;
use crate::dashboard::store::py::{Options as StoreOptions, PyStore};
use crate::pyjson_load::PyValue;
use crate::pystr::{is_space, splitlines, strip};
use crate::pyval::get;
use crate::spec_drift::Outcome;

/// The session the scripts default to (`ARCH_Q_SESSION` unset).
pub const DEFAULT_SESSION: &str = "tm-architect-questions";
/// `run_action`'s `timeout=240`.
const ACTION_TIMEOUT: Duration = Duration::from_secs(240);
/// `settle`'s default `tries`.
const TRIES: usize = 4;

/// What one pass does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start the session (`start-architect-questions.sh <ids>`).
    Start,
    /// Turn Remote Control on (`rc-restore.sh <session>`).
    RcOn,
    /// Turn Remote Control off (`rc-disconnect.sh <session>`).
    RcOff,
    /// Nothing.
    None,
}

impl Action {
    /// The script's name for the action.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::RcOn => "rc_on",
            Self::RcOff => "rc_off",
            Self::None => "none",
        }
    }
}

/// Where the port reads and writes, and how it reaches tmux.
pub struct Options<'a> {
    /// The script's `ROOT`: the Architect checkout.
    pub root: PathBuf,
    /// The script's `HERE`: the action scripts' directory. Empty runs them by
    /// bare name (the replay arm's `PATH` fakes).
    pub scripts: PathBuf,
    /// `dashboard_store.DEFAULT_ROOT`.
    pub store_root: PathBuf,
    /// `dashboard_schema.SCHEMA_DIR`.
    pub schema_dir: PathBuf,
    /// The working directory relative paths resolve against.
    pub cwd: PathBuf,
    /// The environment the script reads.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The tmux runner.
    pub tmux: TmuxCli,
    /// Wall clock and sleep.
    pub clock: Arc<dyn Clock + Send + Sync>,
}

impl<'a> Options<'a> {
    /// The live layout under `checkout`, with [`TmuxCli::live`].
    ///
    /// # Errors
    /// [`QuestionsError::NoTmux`] when no tmux is installed in a known place.
    pub fn live(
        checkout: &Path,
        env: &'a dyn Fn(&str) -> Option<String>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<Self, QuestionsError> {
        Ok(Self {
            root: checkout.to_path_buf(),
            scripts: checkout.join("scripts"),
            store_root: checkout.join("local").join("owner-queue"),
            schema_dir: checkout.join("schemas").join("dashboard"),
            cwd: checkout.to_path_buf(),
            env,
            tmux: TmuxCli::live()?,
            clock,
        })
    }

    /// `SESSION`: `ARCH_Q_SESSION` when set (even empty), else the default.
    pub fn session(&self) -> String {
        (self.env)("ARCH_Q_SESSION").unwrap_or_else(|| DEFAULT_SESSION.into())
    }

    /// `QUEUE`: `ARCH_Q_QUEUE` when set, else `<root>/inbox/interactive-queue.txt`.
    pub fn queue(&self) -> PathBuf {
        (self.env)("ARCH_Q_QUEUE").map_or_else(
            || self.root.join("inbox").join("interactive-queue.txt"),
            PathBuf::from,
        )
    }

    /// `state_path()`: `QUESTIONS_RC_STATE` when non-empty, else `<root>/inbox/questions-rc.json`.
    pub fn state_path(&self) -> PathBuf {
        super::env_or(self.env, "QUESTIONS_RC_STATE").map_or_else(
            || self.root.join("inbox").join("questions-rc.json"),
            PathBuf::from,
        )
    }
}

/// `open_ids(statuses, queued)`: every interactive item, plus queued ids
/// whose item is open or interactive; sorted, no duplicates.
pub fn open_ids(statuses: &BTreeMap<String, Option<String>>, queued: &[String]) -> Vec<String> {
    let is = |s: Option<&Option<String>>, any: &[&str]| {
        s.and_then(Option::as_deref)
            .is_some_and(|s| any.contains(&s))
    };
    let mut ids: Vec<String> = statuses
        .iter()
        .filter(|(_, s)| s.as_deref() == Some("interactive"))
        .map(|(i, _)| i.clone())
        .chain(
            queued
                .iter()
                .filter(|i| is(statuses.get(*i), &["open", "interactive"]))
                .cloned(),
        )
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// `decide(count, session_up, rc_on)`.
pub fn decide(count: usize, up: bool, rc_on: bool) -> Action {
    match (count > 0, up, rc_on) {
        (true, false, _) => Action::Start,
        (true, true, false) => Action::RcOn,
        (false, true, true) => Action::RcOff,
        _ => Action::None,
    }
}

/// `rc_from_capture(lines)`: a line among the last 12 ends in `/rc` after
/// the line start, whitespace, `|` or `·` (`/rc failed` is off).
pub fn rc_from_capture(lines: &[&str]) -> bool {
    let tail = &lines[lines.len().saturating_sub(12)..];
    tail.iter().any(|l| {
        l.match_indices("/rc").any(|(i, _)| {
            let before = l[..i].chars().next_back();
            before.is_none_or(|c| is_space(c) || c == '|' || c == '·')
                && l[i + 3..].chars().all(is_space)
        })
    })
}

/// `url_from_capture(lines)`: the last `https://claude.ai/code/session_<alnum>+`
/// on the pane, or empty.
pub fn url_from_capture(lines: &[&str]) -> String {
    const PREFIX: &str = "https://claude.ai/code/session_";
    let text = lines.join("\n");
    let mut last = String::new();
    for (i, _) in text.match_indices(PREFIX) {
        let tail = &text[i + PREFIX.len()..];
        let n = tail
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(tail.len());
        if n > 0 {
            last = text[i..i + PREFIX.len() + n].to_owned();
        }
    }
    last
}

/// `settle(action, ...)`: re-read until the pane shows the wanted Remote
/// Control state, the session is gone, or `tries` waits of 2 s pass;
/// returns the last `(up, rc_on)` read.
///
/// # Errors
/// The first failed read; no later read runs.
pub fn settle(
    action: Action,
    mut observe: impl FnMut() -> Result<(bool, bool), QuestionsError>,
    clock: &dyn Clock,
    tries: usize,
) -> Result<(bool, bool), QuestionsError> {
    let want = match action {
        Action::RcOn => Some(true),
        Action::RcOff => Some(false),
        _ => None,
    };
    let (mut up, mut rc) = observe()?;
    for _ in 0..tries {
        if want.is_none_or(|w| rc == w) || !up {
            break;
        }
        clock.sleep(Duration::from_secs(2));
        (up, rc) = observe()?;
    }
    Ok((up, rc))
}

/// `write_state(rc, url, count, path)`: the JSON document, written to
/// `<path>.tmp` and renamed over `path`; returns the document text.
///
/// # Errors
/// [`QuestionsError::Write`]; the old file stays.
pub fn write_state(
    rc: bool,
    url: &str,
    count: usize,
    path: &Path,
    clock: &dyn Clock,
) -> Result<String, QuestionsError> {
    let time = crate::classify::alerts::iso(clock.now_unix().floor());
    let doc = format!(
        "{{\"rc\": \"{}\", \"url\": {}, \"count\": {count}, \"time\": \"{time}\"}}",
        if rc { "on" } else { "off" },
        crate::pyval::dumps(&PyValue::Str(url.to_owned()), None, true)
    );
    let shown = path.to_string_lossy().into_owned();
    let werr = |source| QuestionsError::Write {
        path: shown.clone(),
        source,
    };
    // `os.makedirs(os.path.dirname(p))`: a bare file name has no directory to make.
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    std::fs::create_dir_all(dir.ok_or_else(|| werr(std::io::ErrorKind::NotFound.into()))?)
        .map_err(werr)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, &doc).map_err(werr)?;
    std::fs::rename(&tmp, path).map_err(werr)?;
    Ok(doc)
}

/// `read_queue(path)`: stripped non-empty lines (universal newlines); an
/// unreadable file is an empty queue, as the script's `except OSError`.
///
/// # Errors
/// [`QuestionsError::Read`] for a file that is not UTF-8 (the script raises).
pub fn read_queue(path: &Path) -> Result<Vec<String>, QuestionsError> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| QuestionsError::Read(format!("{}: not UTF-8", path.display())))?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    Ok(text
        .split('\n')
        .map(strip)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// `read_statuses()`: item id to its `status` string (`None` for a missing
/// or non-string status).
fn read_statuses(o: &Options<'_>) -> Result<BTreeMap<String, Option<String>>, QuestionsError> {
    let read = |e: crate::dashboard::store::py::PyStoreError| {
        QuestionsError::Read(format!("{}: {e}", e.class()))
    };
    let store = PyStore::new(
        &o.store_root.to_string_lossy(),
        StoreOptions::default(),
        o.env,
        &o.cwd,
        o.schema_dir.clone(),
        o.clock.clone(),
    )
    .map_err(read)?;
    let docs = store
        .list_docs(&PyValue::Str("items".into()), None, None, false, None)
        .map_err(read)?;
    Ok(docs
        .into_iter()
        .map(|d| {
            let status = match get(&d.data, "status") {
                Some(PyValue::Str(s)) => Some(s.clone()),
                _ => None,
            };
            (d.id, status)
        })
        .collect())
}

/// `rc_active(session)`.
fn rc_active(tmux: &TmuxCli, session: &str) -> Result<bool, QuestionsError> {
    Ok(rc_from_capture(&splitlines(&tmux.capture(session)?)))
}

/// `run_action(action, ids)`: the action's script, bounded by 240 s; its exit
/// code, which the script ignores (the settle read decides the state).
///
/// # Errors
/// The script cannot start or times out.
fn run_action(o: &Options<'_>, action: Action, ids: &[String]) -> Result<i32, QuestionsError> {
    let (name, args) = match action {
        Action::Start => ("start-architect-questions.sh", ids.to_vec()),
        Action::RcOn => ("rc-restore.sh", vec![o.session()]),
        Action::RcOff => ("rc-disconnect.sh", vec![o.session()]),
        Action::None => return Ok(0),
    };
    let mut cmd = Command::new(o.scripts.join(name));
    cmd.args(args);
    Ok(run_command(cmd, ACTION_TIMEOUT)?.0)
}

/// One pass; `(stdout line, error)`. The line is printed before the write.
fn pass(o: &Options<'_>, dry: bool, out: &mut String) -> Result<(), QuestionsError> {
    let ids = open_ids(&read_statuses(o)?, &read_queue(&o.queue())?);
    let s = o.session();
    let up = o.tmux.has_session(&s)?;
    let mut rc_on = up && rc_active(&o.tmux, &s)?;
    let action = decide(ids.len(), up, rc_on);
    let mut up = up;
    if !dry {
        run_action(o, action, &ids)?;
        // The script's own first argument to settle is read and then discarded.
        o.tmux.has_session(&s)?;
        let observe = || {
            let up = o.tmux.has_session(&s)?;
            let rc = o.tmux.has_session(&s)? && rc_active(&o.tmux, &s)?;
            Ok((up, rc))
        };
        (up, rc_on) = settle(action, observe, o.clock.as_ref(), TRIES)?;
    }
    let url = if rc_on {
        url_from_capture(&splitlines(&o.tmux.capture(&s)?))
    } else {
        String::new()
    };
    let word = |b: bool, y: &'static str, n: &'static str| if b { y } else { n };
    out.push_str(&format!(
        "questions_rc: count={} action={} session={} rc={}\n",
        ids.len(),
        action.as_str(),
        word(up, "up", "down"),
        word(rc_on, "on", "off")
    ));
    if !dry {
        write_state(rc_on, &url, ids.len(), &o.state_path(), o.clock.as_ref())?;
    }
    Ok(())
}

/// Why: the drop-in for `questions_rc.py [--dry-run]`, the poller's and the
/// dashboard's entry, and the launcher's when `ARCH_Q_RC` is unset.
/// What: see the module doc. Any argument but `--dry-run` is ignored. A
/// failure prints `questions_rc: <error>` and exits 1, after any line
/// already printed; the state file is written only after every read and the
/// action succeeded.
/// Test: `questions_rc_recordings_replay_clean_against_the_port`,
/// `rc_fails_closed_on_a_failed_tmux_call_or_write`.
pub fn run(args: &[String], o: &Options<'_>) -> Outcome {
    let dry = args.iter().any(|a| a == "--dry-run");
    let mut stdout = String::new();
    match pass(o, dry, &mut stdout) {
        Ok(()) => Outcome {
            stdout,
            stderr: String::new(),
            code: 0,
        },
        Err(e) => Outcome {
            stdout,
            stderr: format!("questions_rc: {e}\n"),
            code: 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_matches_the_script_table() {
        use Action::*;
        for (count, up, rc, want) in [
            (2, false, false, Start),
            (2, true, false, RcOn),
            (2, true, true, None),
            (0, true, true, RcOff),
            (0, true, false, None),
            (0, false, false, None),
        ] {
            assert_eq!(decide(count, up, rc), want, "{count} {up} {rc}");
        }
    }

    #[test]
    fn rc_failed_is_off_and_url_is_the_newest_on_the_pane() {
        assert!(rc_from_capture(&["x", "  bypass permissions on · /rc"]));
        assert!(!rc_from_capture(&[
            "x",
            "  bypass permissions on · /rc failed"
        ]));
        assert!(!rc_from_capture(&[]));
        let lines = [
            "a https://claude.ai/code/session_OLD",
            "b https://claude.ai/code/session_NEW",
        ];
        assert!(url_from_capture(&lines).ends_with("NEW"));
        assert_eq!(url_from_capture(&["nothing"]), "");
    }
}
