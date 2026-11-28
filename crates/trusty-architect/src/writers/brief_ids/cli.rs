//! The `brief-id.py` command line over [`super`] (#12028 P1e).
//!
//! Why: four skills call `brief-id.py next | assign | find | list | check`;
//! the port is their drop-in once the replay gate passes.
//! What: [`run`] parses argv as the script's argparse does (a required
//! subcommand) and prints what `main()` prints: the next id, `ID\tassigned`
//! or `ID\tkept` and the path, the registry path of an id (`B` and `MB`
//! forms of one number match), the registry rows, or the problems and their
//! count (exit 1 when any). An exception ends the run with its traceback's
//! last line on stderr and exit 1.
//! Test: the `brief-id` recordings in `tests/replay_writers.rs`,
//! `shipped_brief_id_assigns_in_the_working_directory`.

use std::path::Path;

use super::{Paths, allocate, assign, check, field, id_like, number, read_registry};
use crate::Clock;
use crate::argparse::{HELP, Nargs, Opt, Pos};
use crate::argparse_sub::{self, Cli, Cmd, Sub};
use crate::pyjson_load::PyValue;
use crate::pyval::{py_str, type_name};
use crate::writers::{Outcome, outcome};

const fn plain(name: &'static str, usage: &'static str, help: &'static str) -> Sub {
    Sub {
        name,
        opts: &[HELP],
        pos: &[],
        usage,
        help,
    }
}

const ASSIGN_OPTS: [Opt; 2] = [
    HELP,
    Opt {
        long: "--project",
        short: None,
        value: true,
    },
];

const SUBS: [Sub; 5] = [
    plain(
        "next",
        "usage: brief-id.py next [-h]\n",
        "usage: brief-id.py next [-h]\n\noptions:\n  -h, --help  show this help message and exit\n",
    ),
    Sub {
        name: "assign",
        opts: &ASSIGN_OPTS,
        pos: &[Pos {
            name: "path",
            nargs: Nargs::One,
            choices: &[],
            required: true,
        }],
        usage: "usage: brief-id.py assign [-h] [--project PROJECT] path\n",
        help: "usage: brief-id.py assign [-h] [--project PROJECT] path\n\npositional arguments:\n  path\n\noptions:\n  -h, --help         show this help message and exit\n  --project PROJECT\n",
    },
    Sub {
        name: "find",
        opts: &[HELP],
        pos: &[Pos {
            name: "id",
            nargs: Nargs::One,
            choices: &[],
            required: true,
        }],
        usage: "usage: brief-id.py find [-h] id\n",
        help: "usage: brief-id.py find [-h] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help  show this help message and exit\n",
    },
    plain(
        "list",
        "usage: brief-id.py list [-h]\n",
        "usage: brief-id.py list [-h]\n\noptions:\n  -h, --help  show this help message and exit\n",
    ),
    plain(
        "check",
        "usage: brief-id.py check [-h]\n",
        "usage: brief-id.py check [-h]\n\noptions:\n  -h, --help  show this help message and exit\n",
    ),
];

const TOP_USAGE: &str = "usage: brief-id.py [-h] {next,assign,find,list,check} ...\n";

const CLI: Cli = Cli {
    prog: "brief-id.py",
    usage: TOP_USAGE,
    help: "usage: brief-id.py [-h] {next,assign,find,list,check} ...

brief-id.py: durable brief ids (MB + 4 hex digits, never reused; legacy B ids
still read). Library: scripts/brief_ids.py.

positional arguments:
  {next,assign,find,list,check}

options:
  -h, --help            show this help message and exit
",
    top: &[HELP],
    dest: "cmd",
    names: &["next", "assign", "find", "list", "check"],
    required: true,
    subs: &SUBS,
};

/// Runs the script's command line on `args`, with `REPO` at `paths.repo`.
///
/// Why: one body for the shipped binary and the replay entry point (D6a).
/// What: parse, then the subcommand; `clock` stamps a new registry row.
/// Test: the `brief-id` recordings in `tests/replay_writers.rs`.
pub fn run(args: &[String], paths: &Paths, cwd: &Path, clock: &dyn Clock) -> Outcome {
    let a = match argparse_sub::parse(&CLI, args, &|_, _, _| Ok(())) {
        Cmd::Run(i, a) => (i, a),
        Cmd::Help(text) => return outcome(text.into(), String::new(), 0),
        Cmd::Error(e) => return outcome(String::new(), e, 2),
        Cmd::None => return outcome(String::new(), String::new(), 0),
    };
    let one = |a: &crate::argparse::Args| {
        a.pos[0]
            .as_ref()
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    let result = match a.0 {
        0 => allocate(paths).map(|id| (format!("{id}\n"), 0)),
        1 => {
            let path = one(&a.1);
            let project = a.1.values[1].clone().unwrap_or_else(|| "architect".into());
            assign(&path, &project, paths, cwd, clock).map(|(id, created)| {
                let verb = if created { "assigned" } else { "kept" };
                (format!("{id}\t{verb}\t{path}\n"), 0)
            })
        }
        2 => return find(&one(&a.1), paths),
        3 => list(paths).map(|o| (o, 0)),
        _ => check(paths, cwd).map(|probs| {
            let mut o: String = probs.iter().map(|p| format!("{p}\n")).collect();
            o += &format!("{} problem(s)\n", probs.len());
            (o, i32::from(!probs.is_empty()))
        }),
    };
    match result {
        Ok((o, code)) => outcome(o, String::new(), code),
        Err(e) => outcome(String::new(), format!("{e}\n"), 1),
    }
}

/// `find`: the first registry row whose id equals the argument caselessly,
/// or (both being brief ids) has the same number.
fn find(want_raw: &str, paths: &Paths) -> Outcome {
    let fail = |e: String| outcome(String::new(), format!("{e}\n"), 1);
    let rows = match read_registry(&paths.registry()) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let want = want_raw.to_lowercase();
    let mut hit: Option<&PyValue> = None;
    for r in &rows {
        let id = match field(r, "id") {
            Ok(PyValue::Str(id)) => id,
            Ok(other) => {
                let t = type_name(other);
                return fail(format!(
                    "AttributeError: '{t}' object has no attribute 'lower'"
                ));
            }
            Err(e) => return fail(e),
        };
        let same = id.to_lowercase() == want;
        let by_number =
            !same && id_like(want_raw, false) && id_like(id, false) && number(id) == number(&want);
        if (same || by_number) && hit.is_none() {
            hit = Some(r);
        }
    }
    let Some(r) = hit else {
        return outcome(String::new(), format!("not found: {want_raw}\n"), 1);
    };
    match field(r, "path") {
        Ok(PyValue::Str(p)) if p.starts_with('/') => outcome(format!("{p}\n"), String::new(), 0),
        Ok(PyValue::Str(p)) => {
            let repo = paths.repo.to_string_lossy();
            let sep = if repo.ends_with('/') { "" } else { "/" };
            outcome(format!("{repo}{sep}{p}\n"), String::new(), 0)
        }
        Ok(other) => fail(format!(
            "TypeError: expected str, bytes or os.PathLike object, not {}",
            type_name(other)
        )),
        Err(e) => fail(e),
    }
}

/// `list`: one `id\tproject\tpath\ttitle` line per registry row.
fn list(paths: &Paths) -> Result<String, String> {
    let mut out = String::new();
    for r in read_registry(&paths.registry())? {
        let f = |k: &str| field(&r, k).map(py_str);
        out += &format!(
            "{}\t{}\t{}\t{}\n",
            f("id")?,
            f("project")?,
            f("path")?,
            f("title")?
        );
    }
    Ok(out)
}
