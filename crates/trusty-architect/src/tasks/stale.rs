//! The port of `stale-tasks.py` and the `stale_ranked` rule of `dashboard_derive.py` (#12028 P1d).
//!
//! Why: the Architect parks idle dashboard tasks; the list it reads must name
//! the same tasks, in the same order, with the same text as the script.
//! What: the rows are `stale_ranked(items, goals, now, hours)`, which the
//! `dashboard_derive` port owns (`dashboard::pyderive`, #12029 P2b). [`run`] is
//! the command line: `--root` (else `$DASHBOARD_ROOT`, else the default
//! root), `--hours` (float), `--json`; exit 0, or 2 on a store error.
//! Known gap: an `--hours` that `timedelta` refuses (NaN, infinite, past
//! 999,999,999 days) and a list or dict `goal` make the script print a
//! traceback; the port prints its last line and exits 1.
//! Test: `stale_tasks_recordings_replay_clean_against_the_port`,
//! `shipped_stale_tasks_command_reads_the_working_directory_store`.

use std::path::PathBuf;

use crate::Clock;
use crate::argparse::{self, HELP, Opt, Parsed};
// #12029 P2b: the rules live in the `dashboard_derive` port.
use crate::dashboard::pyderive::stale::{Row, ranked};
use crate::dashboard::store::Format;
use crate::poller::pyfmt::{py_float, py_float_repr};
use crate::pyjson_load::PyValue;
use crate::pystr::py_repr;
use crate::pyval::{dumps, py_str};
use crate::spec_drift::Outcome;

use super::store_read::{abspath, format_error, list_docs};

/// `derive.STALE_HOURS`.
pub const STALE_HOURS: f64 = 24.0;

/// What the caller supplies besides argv and the clock.
#[derive(Debug, Clone)]
pub struct Options {
    /// The program name argparse prints.
    pub prog: String,
    /// The store when neither `--root` nor `$DASHBOARD_ROOT` names one.
    pub default_root: PathBuf,
    /// `$DASHBOARD_ROOT`.
    pub env_root: Option<String>,
    /// `$DASHBOARD_FORMAT`.
    pub dashboard_format: Option<String>,
}

impl Row {
    /// The tab-separated text line.
    fn line(&self) -> String {
        let flags = if self.flags.is_empty() {
            "-".to_owned()
        } else {
            self.flags.join(",")
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.id,
            self.session,
            self.last.as_deref().unwrap_or("never"),
            self.goal.as_ref().map_or_else(|| "-".to_owned(), py_str),
            self.rank
                .as_ref()
                .map_or_else(|| "-".to_owned(), crate::pyval::int_text),
            self.title,
            self.days.map_or_else(|| "-".to_owned(), py_float_repr),
            self.kind,
            flags
        )
    }
}

const OPTS: [Opt; 4] = [
    HELP,
    Opt {
        long: "--root",
        short: None,
        value: true,
    },
    Opt {
        long: "--hours",
        short: None,
        value: true,
    },
    Opt {
        long: "--json",
        short: None,
        value: false,
    },
];

fn usage(prog: &str) -> String {
    argparse::usage(prog, "[-h] [--root ROOT] [--hours HOURS] [--json]")
}

fn help(prog: &str) -> String {
    format!(
        "{}\nstale-tasks.py: list open, unparked dashboard tasks (kind task) with no recent\nactivity.\n\n\
         options:\n  -h, --help     show this help message and exit\n  --root ROOT\n  --hours HOURS\n  \
         --json         print rows as JSON\n",
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

/// Why: the drop-in for the script's command line.
/// What: parses argv like argparse, reads the store and prints the rows
/// (tab-separated, or `json.dumps(rows, indent=1)` with `--json`).
/// Test: `stale_tasks_recordings_replay_clean_against_the_port`.
pub fn run(args: &[String], opts: &Options, clock: &dyn Clock) -> Outcome {
    let check = |i: usize, v: &str| match (i, py_float(v)) {
        (2, None) => Err(format!(
            "argument --hours: invalid float value: {}",
            py_repr(v)
        )),
        _ => Ok(()),
    };
    let a = match argparse::parse_checked(args, &OPTS, false, &check) {
        Parsed::Run(a) => a,
        Parsed::Help => return out(help(&opts.prog), String::new(), 0),
        Parsed::Error(msg) => {
            return out(
                String::new(),
                argparse::error(&usage(&opts.prog), &opts.prog, &msg),
                2,
            );
        }
    };
    let hours = a.values[2]
        .as_deref()
        .and_then(py_float)
        .unwrap_or(STALE_HOURS);
    let root = a.values[1]
        .clone()
        .or_else(|| opts.env_root.clone())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| opts.default_root.to_string_lossy().into_owned());
    let fail = |e: String| {
        out(
            String::new(),
            format!("stale-tasks: cannot read store: {e}\n"),
            2,
        )
    };
    if !std::path::Path::new(&root).is_dir() {
        return fail(format!("no dashboard store at {root}"));
    }
    let format = match opts.dashboard_format.as_deref() {
        None | Some("" | "dir") => Format::Dir,
        Some("jsonl") => Format::Jsonl,
        Some(v) => return fail(format_error(v)),
    };
    let abs = abspath(&root);
    let docs =
        list_docs(&abs, format, "items").and_then(|i| Ok((i, list_docs(&abs, format, "goals")?)));
    let (items, goals) = match docs {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let rows = match ranked(&items, &goals, clock.now_unix(), hours) {
        Ok(r) => r,
        Err(last_line) => return out(String::new(), format!("{last_line}\n"), 1),
    };
    if a.flags[3] {
        let list = PyValue::List(rows.iter().map(Row::to_py).collect());
        return out(
            format!("{}\n", dumps(&list, Some(1), true)),
            String::new(),
            0,
        );
    }
    out(rows.iter().map(Row::line).collect(), String::new(), 0)
}
