//! `main()` of `ruling_pack.py` (#12028 P1i).
//!
//! Why: the binary and the replay entry point share one command line, so a
//! clean replay proves the shipped behaviour.
//! What: [`run`] parses the arguments the way argparse does, loads the key
//! map (`--keys`, else `<checkout>/local/project-keys.json`), builds the
//! pack, prints it, and unless `--no-write` writes it under
//! `<checkout>/local/packs` and prints its path. An unknown project (or a
//! key map without `projects`) is `ruling_pack: <msg>`, exit 2; an exception
//! the script does not catch is its traceback's last line, exit 1. A run that
//! needs the checkout and has none exits 1 before any backend call.
//! Test: `tests/replay_ruling_pack.rs`, `tests/ruling_pack_smoke.rs`.

use std::path::PathBuf;

use super::pack::{BuildError, Request, build, write_pack};
use super::pyerr::{Key, PyErr, subscript, universal};
use crate::argparse::{self, HELP, Nargs, Opt, Parsed, Pos, invalid_choice};
use crate::clock::Clock;
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::{digit_value, py_repr, strip};
use crate::pyval::{get, type_name};
use crate::spec_drift::Outcome;
use crate::writers::{no_checkout, os_error};

/// The script's name in its own messages.
pub const PROG: &str = "ruling_pack.py";

const TYPES: [&str; 5] = ["merge", "prod-write", "spec", "decision", "waiver"];

const fn opt(long: &'static str, value: bool) -> Opt {
    Opt {
        long,
        short: None,
        value,
    }
}

const OPTS: [Opt; 9] = [
    HELP,
    opt("--issue", true),
    opt("--pr", true),
    opt("--file", true),
    opt("--symbol", true),
    opt("--type", true),
    opt("--checked", false),
    opt("--keys", true),
    opt("--no-write", false),
];

const fn pos(name: &'static str) -> Pos {
    Pos {
        name,
        nargs: Nargs::One,
        choices: &[],
        required: true,
    }
}

const POS: [Pos; 2] = [pos("project"), pos("question")];

const USAGE: &str = "usage: ruling_pack.py [-h] [--issue ISSUE] [--pr PR] [--file FILE]
                      [--symbol SYMBOL]
                      [--type {merge,prod-write,spec,decision,waiver}]
                      [--checked] [--keys KEYS] [--no-write]
                      project question
";

const HELP_BODY: &str = "
Build a ruling context pack.

positional arguments:
  project
  question

options:
  -h, --help            show this help message and exit
  --issue ISSUE
  --pr PR
  --file FILE
  --symbol SYMBOL
  --type {merge,prod-write,spec,decision,waiver}
  --checked             the direct read-only check was run and agrees
  --keys KEYS
  --no-write
";

/// What one run reads besides its arguments.
pub struct Options<'a> {
    /// The Architect checkout: the script's `ROOT`, home of the default key
    /// map and the pack directory.
    pub checkout: Option<PathBuf>,
    /// The run's clock; the pack header and file name stamp its instant.
    pub clock: &'a dyn Clock,
}

/// Python `int(s)` as text (`str(int(s))`): surrounding whitespace, a sign,
/// Unicode decimal digits and single underscores between digits.
fn py_int(s: &str) -> Option<String> {
    let t = strip(s);
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut digits = String::new();
    let mut prev_digit = false;
    for c in body.chars() {
        match (c, digit_value(c)) {
            (_, Some(d)) => {
                digits.push(char::from(b'0' + d as u8));
                prev_digit = true;
            }
            ('_', None) if prev_digit => prev_digit = false,
            _ => return None,
        }
    }
    if digits.is_empty() || !prev_digit {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    Some(match (neg, digits) {
        (_, "") => "0".into(),
        (true, d) => format!("-{d}"),
        (false, d) => d.into(),
    })
}

/// argparse's `type=` and `choices` checks, per option index.
fn check(i: usize, v: &str) -> Result<(), String> {
    match i {
        1 | 2 if py_int(v).is_none() => Err(format!(
            "argument {}: invalid int value: {}",
            OPTS[i].long,
            py_repr(v)
        )),
        5 if !TYPES.contains(&v) => Err(invalid_choice("--type", v, &TYPES)),
        _ => Ok(()),
    }
}

/// The key map's top-level setting naming the entry field that adds the
/// `cto` palace (#12097). The script ignores it; unset, no field is checked.
pub(crate) const CTO_FLAG_KEY: &str = "cto_flag_field";

/// The `projects` map and the configured CTO flag field.
type KeyMap = (Vec<(String, PyValue)>, Option<String>);

/// `load_keys(path)`: the `projects` map, or the error `main()` meets, and
/// the string value of [`CTO_FLAG_KEY`] when the map sets one.
///
/// Test: `load_keys_reads_the_cto_flag_field_from_the_key_map`.
pub(crate) fn load_keys(path: &str) -> Result<KeyMap, BuildError> {
    let fatal = |e: PyErr| BuildError::Fatal(e);
    let bytes = std::fs::read(path).map_err(|e| fatal(PyErr::from_line(&os_error(&e, path))))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        fatal(PyErr::new(
            "UnicodeDecodeError",
            "'utf-8' codec can't decode the key map",
        ))
    })?;
    let doc = loads(&universal(&text))
        .map_err(|e| fatal(PyErr::new("json.decoder.JSONDecodeError", e.py_str())))?;
    let projects = match subscript(&doc, Key::Str("projects")) {
        Ok(p) => p,
        Err(e) if e.class == "KeyError" => return Err(BuildError::Unknown("projects".into())),
        Err(e) => return Err(fatal(e)),
    };
    // #12097: read after `projects`, so every error path stays the script's.
    let cto_flag = match get(&doc, CTO_FLAG_KEY) {
        Some(PyValue::Str(f)) if !f.is_empty() => Some(f.clone()),
        _ => None,
    };
    match projects {
        PyValue::Dict(pairs) => Ok((pairs, cto_flag)),
        other => Err(fatal(PyErr::new(
            "AttributeError",
            format!("'{}' object has no attribute 'items'", type_name(&other)),
        ))),
    }
}

/// Runs `ruling_pack.py` with `args` (argv after the program name).
///
/// Why: the shared entry of the shipped binary and `replay_port ruling_pack`.
/// What: see the module doc.
/// Test: `tests/replay_ruling_pack.rs`, `tests/ruling_pack_smoke.rs`.
pub fn run(args: &[String], opts: &Options<'_>) -> Outcome {
    let out = |stdout: String, stderr: String, code: i32| Outcome {
        stdout,
        stderr,
        code,
    };
    let a = match argparse::parse_known(args, &OPTS, &POS, &check) {
        Parsed::Help => return out(format!("{USAGE}{HELP_BODY}"), String::new(), 0),
        Parsed::Error(msg) => return out(String::new(), argparse::error(USAGE, PROG, &msg), 2),
        Parsed::Run(a) if !a.extras.is_empty() => {
            let msg = format!("unrecognized arguments: {}", a.extras.join(" "));
            return out(String::new(), argparse::error(USAGE, PROG, &msg), 2);
        }
        Parsed::Run(a) => a,
    };
    let positional = |i: usize| {
        a.pos[i]
            .as_ref()
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    let ints = |i: usize| -> Vec<String> { a.all[i].iter().filter_map(|v| py_int(v)).collect() };
    let (project, question) = (positional(0), positional(1));
    let (issues, prs) = (ints(1), ints(2));
    let no_write = a.flags[8];
    let checkout = opts.checkout.as_deref();
    let hint = "; pass --keys FILE and --no-write to run without one";
    // See #12028 P1i: the default key map and the pack directory are the
    // checkout's, as the script's ROOT; a run needing one without it stops here.
    let keys = match (&a.values[7], checkout) {
        (Some(k), _) => k.clone(),
        (None, Some(c)) => c
            .join("local/project-keys.json")
            .to_string_lossy()
            .into_owned(),
        (None, None) => return no_checkout(PROG, hint),
    };
    if !no_write && checkout.is_none() {
        return no_checkout(PROG, hint);
    }
    let built = load_keys(&keys).and_then(|(projects, cto_flag)| {
        let req = Request {
            project: &project,
            question: &question,
            issues: &issues,
            prs: &prs,
            files: &a.all[3],
            symbols: &a.all[4],
            qtype: a.values[5].as_deref().unwrap_or("decision"),
            checked: a.flags[6],
            cto_flag: cto_flag.as_deref(),
        };
        build(&req, &projects, opts.clock)
    });
    let (text, key) = match built {
        Ok((text, _, key)) => (text, key),
        Err(BuildError::Unknown(msg)) => {
            return out(String::new(), format!("ruling_pack: {msg}\n"), 2);
        }
        Err(BuildError::Fatal(e)) => return out(String::new(), format!("{e}\n"), 1),
    };
    let mut stdout = format!("{text}\n");
    if let (false, Some(c)) = (no_write, checkout) {
        match write_pack(&text, &key, opts.clock, &c.join("local/packs")) {
            Ok(path) => stdout += &format!("\npack file: {path}\n"),
            Err(e) => return out(stdout, format!("{e}\n"), 1),
        }
    }
    out(stdout, String::new(), 0)
}
