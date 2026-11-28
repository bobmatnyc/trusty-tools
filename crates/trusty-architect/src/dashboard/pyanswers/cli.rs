//! `pending-answers.py`'s command line (#12029 P2e).
//!
//! Why: the Architect runs it before each reply so an owner answer is never
//! missed; the port must print the same lines and exit the same way.
//! What: [`run`]: `--root` (else `$DASHBOARD_ROOT`, else the default root),
//! then one `<qid>\t<kind>\t<at>\t<text>` line per pending event, with the
//! script's per-kind text (interactive, nudge, a `status_requested_at`
//! status request, the ledger id, `architect_do`). Exit 1 when any event is
//! listed, 0 when none, 2 on an `OSError` (`pending-answers: cannot read
//! store: <e>` on stderr). Known gap: a `StoreError` (a corrupt document,
//! the other format) ends the script in a traceback; the port prints its
//! last line and exits 1.
//! Test: `pending_answers_recordings_replay_clean_against_the_port`,
//! `interactive_line_matches_the_script`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{ILIO_ACTOR, field, pending, qualified};
use crate::argparse::{self, HELP, Opt, Parsed};
use crate::clock::Clock;
use crate::dashboard::store::py::{Options as StoreOptions, PyStore, PyStoreError};
use crate::pyjson_load::PyValue;
use crate::spec_drift::Outcome;

/// What the caller supplies besides argv.
pub struct Options<'a> {
    /// The program name argparse prints.
    pub prog: String,
    /// `ds.DEFAULT_ROOT`.
    pub default_root: PathBuf,
    /// The working directory a relative root is read against.
    pub cwd: PathBuf,
    /// The process environment (`DASHBOARD_ROOT`, `DASHBOARD_FORMAT`, `DASHBOARD_ACTOR`, ...).
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// `dashboard_schema.SCHEMA_DIR`.
    pub schema_dir: PathBuf,
    /// `datetime.now(timezone.utc)`.
    pub clock: Arc<dyn Clock + Send + Sync>,
}

const OPTS: [Opt; 2] = [
    HELP,
    Opt {
        long: "--root",
        short: None,
        value: true,
    },
];

fn usage(prog: &str) -> String {
    argparse::usage(prog, "[-h] [--root ROOT]")
}

fn help(prog: &str) -> String {
    format!(
        "{}\npending-answers.py: list dashboard items Bob answered, cancelled or marked\n\
         Won't do (kind \"wont_do\",\n\noptions:\n  -h, --help   show this help message and exit\n  \
         --root ROOT\n",
        usage(prog)
    )
}

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// `it.get(k)` as text, `""` when absent.
fn f(it: &PyValue, k: &str) -> String {
    field(it, k).unwrap_or_default()
}

/// The text column of one event (`main`'s per-kind rules).
fn line_text(it: &PyValue) -> String {
    let kind = f(it, "kind");
    let mut text = if kind == "interactive" {
        format!(
            "INTERACTIVE: {} {} -> run scripts/start-architect-questions.sh {}; never a dialog in tm-architect",
            f(it, "id"),
            f(it, "text"),
            f(it, "id")
        )
    } else {
        f(it, "text")
    };
    if kind == "nudge" {
        let to = Some(f(it, "session"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "the owning PM".into());
        text = format!(
            "NUDGE {}: {} (relay to {to}: check in and move it; then write nudge_cleared_at)",
            f(it, "id"),
            f(it, "text")
        );
    }
    if kind == "status_request" && f(it, "via") == "status_requested_at" {
        text = format!(
            "STATUS {}: {} (write a status update, then status_at)",
            f(it, "id"),
            f(it, "text")
        );
    }
    let task = f(it, "task");
    if ["owner_complete", "owner_status_update", "owner_park"].contains(&kind.as_str())
        && !task.is_empty()
    {
        text = format!("{text} (ledger {task})");
    }
    if kind == "architect_do" {
        let or_missing = |v: String| if v.is_empty() { "MISSING".into() } else { v };
        let title = Some(f(it, "title"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| f(it, "text"));
        text = format!(
            "{title} | task {} | session {}",
            or_missing(task),
            or_missing(f(it, "session"))
        );
    }
    text
}

/// `qualified(root, pending(root))`: two stores, as the script opens them.
fn read(root: &str, o: &Options) -> Result<Vec<PyValue>, PyStoreError> {
    let open = || {
        let root = Some(root).filter(|r| !r.is_empty());
        let default = o.default_root.to_string_lossy();
        PyStore::new(
            root.unwrap_or(&default),
            StoreOptions::default(),
            o.env,
            &o.cwd,
            o.schema_dir.clone(),
            o.clock.clone(),
        )
    };
    let trial = (o.env)("DASHBOARD_ACTOR").as_deref() == Some(ILIO_ACTOR);
    let events = pending(&open()?, o.clock.now_unix(), trial)?;
    qualified(&open()?, events, Path::new(&o.default_root))
}

/// Why: the drop-in for the script's command line.
/// What: see the module doc.
/// Test: `pending_answers_recordings_replay_clean_against_the_port`.
pub fn run(args: &[String], o: &Options) -> Outcome {
    let a = match argparse::parse(args, &OPTS, false) {
        Parsed::Run(a) => a,
        Parsed::Help => return out(help(&o.prog), String::new(), 0),
        Parsed::Error(msg) => {
            return out(
                String::new(),
                argparse::error(&usage(&o.prog), &o.prog, &msg),
                2,
            );
        }
    };
    let root = a.values[1]
        .clone()
        .or_else(|| (o.env)("DASHBOARD_ROOT"))
        .unwrap_or_default();
    let items = match read(&root, o) {
        Ok(items) => items,
        // #12029 P2e: only an OSError is caught; nothing is listed, exit 2.
        Err(e @ PyStoreError::Os { .. }) => {
            return out(
                String::new(),
                format!("pending-answers: cannot read store: {e}\n"),
                2,
            );
        }
        Err(e) => return out(String::new(), format!("{}: {e}\n", e.class()), 1),
    };
    let mut stdout = String::new();
    for it in &items {
        stdout.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            f(it, "qid"),
            f(it, "kind"),
            f(it, "at"),
            line_text(it)
        ));
    }
    out(stdout, String::new(), i32::from(!items.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::line_text;
    use crate::pyjson_load::PyValue;

    /// The interactive line names the questions session, which no recording
    /// may hold; the expected text is the frozen script's output on `D0k`.
    #[test]
    fn interactive_line_matches_the_script() {
        let it = PyValue::Dict(
            [
                ("id", "D0k"),
                ("kind", "interactive"),
                ("text", "talk it over"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), PyValue::Str(v.to_owned())))
            .collect(),
        );
        assert_eq!(
            line_text(&it),
            "INTERACTIVE: D0k talk it over -> run scripts/start-architect-questions.sh D0k; \
             never a dialog in tm-architect"
        );
    }
}
