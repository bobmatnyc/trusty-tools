//! The footer's `Reminders (ack): ...` line (#12029 P2g).
//!
//! Why: the owner acks IL reminders from the reply footer (ar-T8d); a
//! reminder fired and not acked must show in every reply until it is acked,
//! and a ledger that cannot be read must be named, never hidden.
//! What: [`reminders_line`]: no `state.json` where `datg.paths(project)` puts
//! it (`$ARCHITECT_IL_DIR`, else `<checkout>/local/il`; `$DATG_STATE_DIR`
//! and `$DATG_SESSION` as `datg.py` reads them) is no line. Otherwise one
//! bounded `datg --project <dir> reminders --due --json` (found on `PATH`,
//! stdin closed) lists the rows, and `il_remind.footer_line` formats them:
//! at most five `<task>#<n> <note[:60]>` joined by ` · `, then `+N more`. A
//! failure is `Reminders: check failed (<Type>: <msg[:80]>)`.
//! Intended divergence (Architect ruling 17:10Z): the frozen script imports
//! `datg` and `il_remind` and reads the state in process; the port reads the
//! same rows through the one subprocess. Known gap: with per-PM state
//! directories and `$TMUX_PANE` set, `datg.py` picks the directory by tmux;
//! the port lets `datg` pick and runs it when any session has a state file.
//! Test: `reminders_line_formats_the_datg_rows_as_il_remind_does`,
//! `reminders_line_names_each_failure`, and the `footer-reminders*`
//! recordings.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::Ctx;
use crate::process::{RunFailure, run_bounded};
use crate::pyjson_load::{PyValue, loads};
use crate::pyval::{get, py_str};

/// How long the `datg` read may run.
const DATG_TIMEOUT: Duration = Duration::from_secs(10);
/// `il_remind.FOOTER_MAX`.
const FOOTER_MAX: usize = 5;
/// `il_remind.FOOTER_NOTE`.
const FOOTER_NOTE: usize = 60;
/// The variables `datg.py` resolves its state directory from.
const DATG_ENV: [&str; 3] = ["DATG_STATE_DIR", "DATG_SESSION", "TMUX_PANE"];

/// `datg.SESSION_RE`: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
fn session_name(s: &str) -> bool {
    let b = s.strip_suffix('\n').unwrap_or(s);
    let mut c = b.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric())
        && b.chars().count() <= 64
        && c.all(|x| x.is_ascii_alphanumeric() || "._-".contains(x))
}

/// Whether `datg.paths(project)[1]` is a file, as far as it resolves without tmux.
fn state_exists(ctx: &Ctx, project: &Path) -> bool {
    let set = |k: &str| (ctx.o.env)(k).filter(|v| !v.is_empty());
    if let Some(forced) = set("DATG_STATE_DIR") {
        return ctx.o.cwd.join(forced).join("state.json").is_file();
    }
    let base = project.join(".trusty-mpm").join("datg");
    if let Some(s) = set("DATG_SESSION").filter(|s| session_name(s))
        && base.join(s).join("state.json").is_file()
    {
        return true;
    }
    let subs = std::fs::read_dir(&base)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().is_dir() && e.path().join("state.json").is_file())
        })
        .unwrap_or(false);
    // Known gap: tmux names the per-PM directory; `datg` resolves it itself.
    if subs && set("TMUX_PANE").is_some() {
        return true;
    }
    base.join("state.json").is_file()
}

/// `type(e).__name__` and `str(e)` of a failed read.
type Failure = (String, String);

/// The rows `datg reminders --due --json` prints, one JSON object per line.
fn datg_rows(ctx: &Ctx, project: &Path) -> Result<Vec<PyValue>, Failure> {
    let mut cmd = Command::new(&ctx.o.datg);
    cmd.arg("--project")
        .arg(project)
        .args(["reminders", "--due", "--json"])
        .current_dir(&ctx.o.cwd);
    for k in DATG_ENV {
        match (ctx.o.env)(k) {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    let shown = ctx.o.datg.to_string_lossy().into_owned();
    let done = run_bounded(cmd, DATG_TIMEOUT).map_err(|f| match f {
        RunFailure::Spawn(e) | RunFailure::Wait(e) => {
            let class = crate::writers::os_error(&e, &shown);
            let (c, m) = class.split_once(": ").unwrap_or(("OSError", &class));
            (c.to_owned(), m.to_owned())
        }
        RunFailure::Timeout => (
            "TimeoutExpired".into(),
            format!(
                "{shown} reminders timed out after {} seconds",
                DATG_TIMEOUT.as_secs()
            ),
        ),
    })?;
    if !done.status.success() {
        let err = String::from_utf8_lossy(&done.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim_end();
        return Err(exit_failure(last, done.status.code()));
    }
    let out = String::from_utf8_lossy(&done.stdout);
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| loads(l).map_err(|e| ("JSONDecodeError".to_owned(), e.py_str())))
        .collect()
}

/// A non-zero `datg` exit as the exception the in-process read raises:
/// `datg: <msg>` is a `DatgError`, a traceback's `<Class>: <msg>` keeps its class.
fn exit_failure(last: &str, code: Option<i32>) -> Failure {
    if let Some(msg) = last.strip_prefix("datg: ") {
        return ("DatgError".into(), msg.to_owned());
    }
    if let Some((class, msg)) = last.split_once(": ")
        && !class.is_empty()
        && class
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        let class = class.rsplit('.').next().unwrap_or(class);
        return (class.to_owned(), msg.to_owned());
    }
    let code = code.map_or("a signal".to_owned(), |c| format!("exit {c}"));
    (
        "DatgError".into(),
        if last.is_empty() {
            code
        } else {
            last.to_owned()
        },
    )
}

/// `x[k]`: a missing key is the `KeyError` the script catches.
fn field(x: &PyValue, k: &str) -> Result<String, Failure> {
    get(x, k)
        .map(py_str)
        .ok_or_else(|| ("KeyError".into(), crate::pystr::py_repr(k)))
}

/// `il_remind.footer_line(rows)`.
fn footer_line(rows: &[PyValue]) -> Result<String, Failure> {
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut parts = Vec::new();
    for x in rows.iter().take(FOOTER_MAX) {
        let note: String = field(x, "note")?.chars().take(FOOTER_NOTE).collect();
        parts.push(format!("{}#{} {note}", field(x, "task")?, field(x, "n")?));
    }
    if rows.len() > FOOTER_MAX {
        parts.push(format!("+{} more", rows.len() - FOOTER_MAX));
    }
    Ok(format!("Reminders (ack): {}", parts.join(" \u{b7} ")))
}

/// `il_remind.il_dir()`: `$ARCHITECT_IL_DIR`, else `<checkout>/local/il`.
fn il_dir(ctx: &Ctx) -> PathBuf {
    (ctx.o.env)("ARCHITECT_IL_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(
            || ctx.o.checkout.join("local").join("il"),
            |d| ctx.o.cwd.join(d),
        )
}

/// Why: see the module doc.
/// What: `reminders_line()`: `""` with no ledger or nothing due, the line, or
/// the `check failed` line.
/// Test: `reminders_line_formats_the_datg_rows_as_il_remind_does`.
pub(crate) fn reminders_line(ctx: &Ctx) -> String {
    let project = il_dir(ctx);
    if !state_exists(ctx, &project) {
        return String::new();
    }
    match datg_rows(ctx, &project).and_then(|rows| footer_line(&rows)) {
        Ok(line) => line,
        Err((class, msg)) => {
            let msg: String = msg.chars().take(80).collect();
            format!("Reminders: check failed ({class}: {msg})")
        }
    }
}
