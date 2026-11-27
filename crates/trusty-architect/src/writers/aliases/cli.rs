//! `aliases.py`'s command line and its one writer, `add` (#12028 P1e).
//!
//! Why: `resolve`, `list`, `check` and `add` are the script's whole contract;
//! `add` rewrites the table, so a failed write must not report success.
//! What: [`run`] parses argv like the script's argparse (a bad `resolve`
//! command line prints the error and still exits 0), then runs the command.
//! `add` loads the table (or starts `{"version": 1}`), merges the entry,
//! refuses a table [`super::check::check`] flags (nothing written), and saves
//! as `_save` does: `json.dump(indent=2, ensure_ascii=False)` plus a newline
//! into `<path>.tmp`, then `os.replace` onto the path.
//! Known gap: a table whose entry lists are not lists ends the script in a
//! traceback; the port prints an approximate last line (exit 1).
//! Test: the `aliases` recordings in `tests/replay_writers.rs`,
//! `shipped_aliases_add_writes_the_table_atomically`.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::check::check;
use super::{describe, entries, name, resolve_line, str_items};
use crate::argparse::{HELP, Nargs, Opt, Pos, invalid_choice};
use crate::argparse_sub::{self, Cli, Cmd, Sub};
use crate::pyjson_load::{PyValue, loads};
use crate::pyval::{dumps, get, truthy, type_name};
use crate::writers::{Outcome, errno_text, outcome, universal_newlines};

/// Where the run finds its table and its input.
pub struct Options<'a> {
    /// `<checkout>/local/aliases.json`, used when `ALIASES_FILE` is unset or empty.
    pub default_path: PathBuf,
    /// `$ALIASES_FILE`.
    pub aliases_file: Option<String>,
    /// Standard input, read by `resolve` without TEXT.
    pub stdin: &'a mut dyn Read,
}

const PROG: &str = "aliases.py";
const TOP_USAGE: &str = "usage: aliases.py [-h] {resolve,list,check,add} ...\n";
const TOP_HELP: &str = "usage: aliases.py [-h] {resolve,list,check,add} ...

aliases.py: deterministic alias table for the owner's nicknames and project
references.

positional arguments:
  {resolve,list,check,add}

options:
  -h, --help            show this help message and exit
";
const KIND_CHOICES: &[&str] = &["host", "person", "project"];

const fn opt(long: &'static str, value: bool) -> Opt {
    Opt {
        long,
        short: None,
        value,
    }
}

const RESOLVE_OPTS: [Opt; 2] = [HELP, opt("--all", false)];
const LIST_OPTS: [Opt; 2] = [HELP, opt("--kind", true)];
const ADD_OPTS: [Opt; 13] = [
    HELP,
    opt("--alias", true),
    opt("--github", true),
    opt("--session", true),
    opt("--repo-path", true),
    opt("--github-repo", true),
    opt("--palace", true),
    opt("--index-id", true),
    opt("--role", true),
    opt("--slack-user-id", true),
    opt("--hostname", true),
    opt("--source", true),
    opt("--confirmed", false),
];
/// `ADD_OPTS` index to entry field, for the scalar options.
const SCALARS: [(usize, &str); 7] = [
    (4, "repo_path"),
    (5, "github_repo"),
    (6, "palace"),
    (7, "index_id"),
    (8, "role"),
    (9, "slack_user_id"),
    (10, "hostname"),
];
const ADD_USAGE: &str = "usage: aliases.py add [-h] [--alias ALIAS] [--github GITHUB]
                      [--session SESSION] [--repo-path REPO_PATH]
                      [--github-repo GITHUB_REPO] [--palace PALACE]
                      [--index-id INDEX_ID] [--role ROLE]
                      [--slack-user-id SLACK_USER_ID] [--hostname HOSTNAME]
                      [--source SOURCE] [--confirmed]
                      {host,person,project} name
";
const SUBS: [Sub; 4] = [
    Sub {
        name: "resolve",
        opts: &RESOLVE_OPTS,
        pos: &[Pos {
            name: "text",
            nargs: Nargs::Optional,
            choices: &[],
            required: false,
        }],
        usage: "usage: aliases.py resolve [-h] [--all] [text]\n",
        help: "usage: aliases.py resolve [-h] [--all] [text]\n\npositional arguments:\n  text\n\noptions:\n  -h, --help  show this help message and exit\n  --all       include quiet (common-word) aliases\n",
    },
    Sub {
        name: "list",
        opts: &LIST_OPTS,
        pos: &[],
        usage: "usage: aliases.py list [-h] [--kind {host,person,project}]\n",
        help: "usage: aliases.py list [-h] [--kind {host,person,project}]\n\noptions:\n  -h, --help            show this help message and exit\n  --kind {host,person,project}\n",
    },
    Sub {
        name: "check",
        opts: &[HELP],
        pos: &[],
        usage: "usage: aliases.py check [-h]\n",
        help: "usage: aliases.py check [-h]\n\noptions:\n  -h, --help  show this help message and exit\n",
    },
    Sub {
        name: "add",
        opts: &ADD_OPTS,
        pos: &[
            Pos {
                name: "kind",
                nargs: Nargs::One,
                choices: KIND_CHOICES,
                required: true,
            },
            Pos {
                name: "name",
                nargs: Nargs::One,
                choices: &[],
                required: true,
            },
        ],
        usage: ADD_USAGE,
        help: ADD_HELP,
    },
];

/// `add -h`: the usage block and the body.
const ADD_HELP: &str = concat!(
    "usage: aliases.py add [-h] [--alias ALIAS] [--github GITHUB]\n",
    "                      [--session SESSION] [--repo-path REPO_PATH]\n",
    "                      [--github-repo GITHUB_REPO] [--palace PALACE]\n",
    "                      [--index-id INDEX_ID] [--role ROLE]\n",
    "                      [--slack-user-id SLACK_USER_ID] [--hostname HOSTNAME]\n",
    "                      [--source SOURCE] [--confirmed]\n",
    "                      {host,person,project} name\n",
    "\n",
    "positional arguments:\n",
    "  {host,person,project}\n",
    "  name\n",
    "\n",
    "options:\n",
    "  -h, --help            show this help message and exit\n",
    "  --alias ALIAS\n",
    "  --github GITHUB\n",
    "  --session SESSION\n",
    "  --repo-path REPO_PATH\n",
    "  --github-repo GITHUB_REPO\n",
    "  --palace PALACE\n",
    "  --index-id INDEX_ID\n",
    "  --role ROLE\n",
    "  --slack-user-id SLACK_USER_ID\n",
    "  --hostname HOSTNAME\n",
    "  --source SOURCE\n",
    "  --confirmed\n",
);

const CLI: Cli = Cli {
    prog: PROG,
    usage: TOP_USAGE,
    help: TOP_HELP,
    dest: "cmd",
    names: &["resolve", "list", "check", "add"],
    required: false,
    subs: &SUBS,
};

/// Why a command failed: the `OSError` / `ValueError` text main prints, or a
/// traceback's last line.
enum Fail {
    /// `print(f"error: {e}")` on stderr, exit 1.
    Caught(String),
    /// An uncaught exception: its last line on stderr, exit 1.
    Raised(String),
    /// `KeyError`: main prints the help, exit 0.
    Key,
}

/// Runs the script's command line on `args` (argv after the program name).
///
/// Why: one body for the shipped binary and the replay entry point (D6a).
/// What: parse, then `resolve`, `list`, `check` or `add`; returns the
/// streams and exit code the script ends with.
/// Test: the `aliases` recordings in `tests/replay_writers.rs`.
pub fn run(args: &[String], opts: &mut Options<'_>) -> Outcome {
    let check_opt = |cmd: usize, o: usize, v: &str| match (cmd, o) {
        (1, 1) if !KIND_CHOICES.contains(&v) => Err(invalid_choice("--kind", v, KIND_CHOICES)),
        _ => Ok(()),
    };
    let path = match opts.aliases_file.as_deref() {
        Some(p) if !p.is_empty() => p.to_owned(),
        _ => opts.default_path.to_string_lossy().into_owned(),
    };
    let result = match argparse_sub::parse(&CLI, args, &check_opt) {
        Cmd::Help(text) => return outcome(text.into(), String::new(), 0),
        Cmd::Error(e) => {
            let code = if args.first().is_some_and(|a| a == "resolve") {
                0
            } else {
                2
            };
            return outcome(String::new(), e, code);
        }
        Cmd::None => Err(Fail::Key),
        Cmd::Run(0, a) => {
            let text = match a.pos[0].as_ref().and_then(|v| v.first()) {
                Some(t) => Some(t.clone()),
                None => read_stdin(opts.stdin),
            };
            let line = text
                .filter(|t| !crate::pystr::strip(t).is_empty())
                .and_then(|t| {
                    load(&path)
                        .ok()
                        .map(|table| resolve_line(&t, &table, a.flags[1]))
                })
                .unwrap_or_default();
            let out = if line.is_empty() {
                line
            } else {
                format!("{line}\n")
            };
            return outcome(out, String::new(), 0);
        }
        Cmd::Run(1, a) => cmd_list(&path, a.values[1].as_deref()),
        Cmd::Run(2, _) => return cmd_check(&path),
        Cmd::Run(_, a) => cmd_add(&path, &a),
    };
    match result {
        Ok((out, err, code)) => outcome(out, err, code),
        Err(Fail::Key) => outcome(TOP_HELP.into(), String::new(), 0),
        Err(Fail::Caught(e)) => outcome(String::new(), format!("error: {e}\n"), 1),
        Err(Fail::Raised(e)) => outcome(String::new(), format!("{e}\n"), 1),
    }
}

/// `sys.stdin.read()`; `None` on a read error (resolve stays silent).
fn read_stdin(stdin: &mut dyn Read) -> Option<String> {
    let mut buf = Vec::new();
    stdin.read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok().map(universal_newlines)
}

/// `load(path)`: the table, or the `OSError` / `ValueError` text.
fn load(path: &str) -> Result<PyValue, String> {
    let bytes = std::fs::read(path).map_err(|e| errno_text(&e, path))?;
    let text =
        String::from_utf8(bytes).map_err(|_| "'utf-8' codec can't decode the file".to_owned())?;
    match loads(&universal_newlines(text)) {
        Ok(t @ PyValue::Dict(_)) => Ok(t),
        Ok(_) => Err("alias table is not an object".into()),
        Err(e) => Err(e.py_str()),
    }
}

type Done = Result<(String, String, i32), Fail>;

fn cmd_list(path: &str, kind: Option<&str>) -> Done {
    let table = load(path).map_err(Fail::Caught)?;
    let mut out = String::new();
    for (k, e) in entries(&table).map_err(Fail::Raised)? {
        if kind.is_some_and(|want| want != k) {
            continue;
        }
        let flag = if get(e, "confirmed").is_some_and(truthy) {
            "ok "
        } else {
            "unc"
        };
        let aliases = join_items(get(e, "aliases")).map_err(Fail::Raised)?;
        let target = describe(k, e).map_err(Fail::Raised)?;
        out += &format!("{flag} {k:7} {} | {aliases} | {target}\n", name(e));
    }
    Ok((out, String::new(), 0))
}

/// `", ".join(v or [])`: every item must be a `str`.
fn join_items(v: Option<&PyValue>) -> Result<String, String> {
    if let Some(PyValue::List(items)) = v {
        for (n, x) in items.iter().enumerate() {
            if !matches!(x, PyValue::Str(_)) {
                let t = type_name(x);
                return Err(format!(
                    "TypeError: sequence item {n}: expected str instance, {t} found"
                ));
            }
        }
    }
    Ok(str_items(v)?.join(", "))
}

fn cmd_check(path: &str) -> Outcome {
    let table = match load(path) {
        Ok(t) => t,
        Err(e) => return outcome(format!("ERROR: cannot read table: {e}\n"), String::new(), 1),
    };
    let (errs, notes) = match check(&table) {
        Ok(r) => r,
        Err(e) => return outcome(String::new(), format!("{e}\n"), 1),
    };
    let mut out = String::new();
    for n in &notes {
        out += &format!("NOTE {n}\n");
    }
    for e in &errs {
        out += &format!("ERROR {e}\n");
    }
    out += &format!("{} errors, {} ambiguous aliases\n", errs.len(), notes.len());
    outcome(out, String::new(), i32::from(!errs.is_empty()))
}

/// `d[key] = v`: replaced in place, or appended.
fn set(d: &mut PyValue, key: &str, v: PyValue) {
    if let PyValue::Dict(pairs) = d {
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = v,
            None => pairs.push((key.to_owned(), v)),
        }
    }
}

/// `d.setdefault(key, [])` as a list; `Err` where the script would raise.
fn list_field<'a>(
    d: &'a mut PyValue,
    key: &str,
    missing_is_key_error: bool,
) -> Result<&'a mut Vec<PyValue>, Fail> {
    if get(d, key).is_none() {
        if missing_is_key_error {
            return Err(Fail::Key);
        }
        set(d, key, PyValue::List(Vec::new()));
    }
    let PyValue::Dict(pairs) = d else {
        return Err(Fail::Key);
    };
    match pairs.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v) {
        Some(PyValue::List(items)) => Ok(items),
        Some(other) => Err(Fail::Raised(format!(
            "AttributeError: '{}' object has no attribute 'append'",
            type_name(other)
        ))),
        None => Err(Fail::Key),
    }
}

fn str_value(s: &str) -> PyValue {
    PyValue::Str(s.to_owned())
}

fn cmd_add(path: &str, a: &crate::argparse::Args) -> Done {
    let pos = |i: usize| {
        a.pos[i]
            .as_ref()
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    let (kind, want) = (pos(0), pos(1));
    let mut table = if Path::new(path).exists() {
        load(path).map_err(Fail::Caught)?
    } else {
        PyValue::Dict(vec![(
            "version".into(),
            PyValue::Int(crate::pyjson_load::PyInt::Small(1)),
        )])
    };
    let key = super::KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("hosts", |(_, t)| t);
    let rows = list_field(&mut table, key, false)?;
    let mut found = None;
    for (i, r) in rows.iter().enumerate() {
        let PyValue::Dict(_) = r else {
            return Err(Fail::Raised(format!(
                "AttributeError: '{}' object has no attribute 'get'",
                type_name(r)
            )));
        };
        let n = match get(r, "name") {
            None => String::new(),
            Some(PyValue::Str(s)) => s.clone(),
            Some(o) => {
                let t = type_name(o);
                return Err(Fail::Raised(format!(
                    "AttributeError: '{t}' object has no attribute 'lower'"
                )));
            }
        };
        if n.to_lowercase() == want.to_lowercase() {
            found = Some(i);
            break;
        }
    }
    let i = match found {
        Some(i) => i,
        None => {
            rows.push(new_entry(&kind, &want, a));
            rows.len() - 1
        }
    };
    let e = &mut rows[i];
    for (opt, field, key_error) in [
        (1, "aliases", true),
        (2, "github", false),
        (3, "sessions", false),
    ] {
        if a.all[opt].is_empty() {
            continue;
        }
        let items = list_field(e, field, key_error)?;
        for v in &a.all[opt] {
            if !items.iter().any(|x| matches!(x, PyValue::Str(s) if s == v)) {
                items.push(str_value(v));
            }
        }
    }
    for (opt, field) in SCALARS {
        if let Some(v) = &a.values[opt] {
            set(e, field, str_value(v));
        }
    }
    if a.flags[12] {
        set(e, "confirmed", PyValue::Bool(true));
    }
    let line = match get(e, "aliases") {
        Some(PyValue::List(items)) => format!("{kind} {}: {} aliases\n", name(e), items.len()),
        _ => String::new(),
    };
    let (errs, _) = check(&table).map_err(Fail::Raised)?;
    if !errs.is_empty() {
        return Ok((
            String::new(),
            format!("not written:\n{}\n", errs.join("\n")),
            1,
        ));
    }
    if line.is_empty() {
        return Err(Fail::Key); // unreachable: a table that passes `check` has the list
    }
    save(&table, path).map_err(Fail::Caught)?;
    Ok((line, String::new(), 0))
}

fn new_entry(kind: &str, want: &str, a: &crate::argparse::Args) -> PyValue {
    let source = a.values[11].as_deref().unwrap_or("added by hand");
    let list = || PyValue::List(Vec::new());
    let mut e = vec![
        ("name", str_value(want)),
        ("aliases", list()),
        ("source", str_value(source)),
        ("confirmed", PyValue::Bool(a.flags[12])),
    ];
    match kind {
        "person" => e.extend([
            ("github", list()),
            ("slack_user_id", PyValue::Null),
            ("email_location", str_value("directory")),
            ("role", str_value("unspecified")),
        ]),
        "project" => e.extend([
            ("sessions", list()),
            ("repo_path", PyValue::Null),
            ("github_repo", PyValue::Null),
            ("palace", PyValue::Null),
            ("index_id", PyValue::Null),
        ]),
        _ => e.extend([
            ("hostname", PyValue::Null),
            ("role", str_value("unspecified")),
        ]),
    }
    PyValue::Dict(e.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// `_save`: the table into `<path>.tmp`, then `os.replace` onto `path`.
fn save(table: &PyValue, path: &str) -> Result<(), String> {
    let tmp = format!("{path}.tmp");
    let text = format!("{}\n", dumps(table, Some(2), false));
    std::fs::write(&tmp, text).map_err(|e| errno_text(&e, &tmp))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let t = errno_text(&e, &tmp);
        format!("{t} -> {}", crate::pystr::py_repr(path))
    })
}
