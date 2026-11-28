//! The port of `verify_claim_input.py`: the event shape and the read-only guard (#12028 P1g).
//!
//! Why: `verify-claim` acts on an event a hook wrote from a PM message, so a
//! malformed one must be refused before any command runs, and no command
//! outside the read-only set may ever run.
//! What: [`parse_event`] validates one decoded event in the script's order
//! and with its messages; [`load_event`] reads it from a file, `-` (stdin) or
//! a literal JSON object. [`guard_read_only`] refuses every command outside
//! `gh pr view|checks`, `gh run view`, a plain `gh api` GET of a
//! `repos/<o>/<r>/pulls|issues|commits/...` path, the read-only git verbs and
//! one crates.io GET. [`tool_is_network`] names gh and curl.
//! Known gap: `str.isdigit()` also accepts digits outside Unicode `Nd`
//! (superscripts), on which the script's `int()` then raises; the port treats
//! those as non-digits.
//! Test: the `badinput-*` and `guard-refusal` recordings in
//! `tests/replay_verify_claim.rs`, `rest_path_follows_the_script_pattern`.

use std::io::Read;
use std::path::Path;

use super::pyops::{PyExc, int_from_digits, lower_py};
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::{is_digit, is_word, lstrip, py_repr};
use crate::pyval::{py_str, repr};
use crate::writers::{read_text, universal_newlines};

/// `KINDS`.
pub const KINDS: [&str; 8] = [
    "clearance_request",
    "merged",
    "published",
    "paused",
    "status",
    "improvement",
    "question",
    "fyi",
];
/// `CRATES_API`.
pub const CRATES_API: &str = "https://crates.io/api/v1/crates";

/// `BadInput`: the event is not a usable event object.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BadInput(pub String);

/// One `claims.issues` entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IssueClaim {
    pub(crate) n: PyInt,
    pub(crate) state: String,
    pub(crate) reason: String,
}

/// A validated event; a string field is `None` for null or absent.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub(crate) kind: String,
    pub(crate) repo: Option<String>,
    pub(crate) pr: Option<PyInt>,
    pub(crate) head: Option<String>,
    pub(crate) base: Option<String>,
    pub(crate) merge_sha: Option<String>,
    pub(crate) crate_name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) ci_run: Option<PyInt>,
    /// The `claims` object (`{}` for null).
    pub(crate) claims: PyValue,
    pub(crate) issues: Vec<IssueClaim>,
}

fn field<'a>(raw: &'a [(String, PyValue)], key: &str) -> &'a PyValue {
    raw.iter()
        .find(|(k, _)| k == key)
        .map_or(&PyValue::Null, |(_, v)| v)
}

fn is_plain_int(v: &PyValue) -> bool {
    matches!(v, PyValue::Int(_))
}

fn int_or_null(raw: &[(String, PyValue)], key: &str) -> Result<Option<PyInt>, BadInput> {
    match field(raw, key) {
        PyValue::Null => Ok(None),
        PyValue::Int(i) => Ok(Some(i.clone())),
        PyValue::Str(s) if !s.is_empty() && s.chars().all(is_digit) => Ok(Some(int_from_digits(s))),
        other => Err(BadInput(format!(
            "{key} must be an integer or null; got {}",
            repr(other)
        ))),
    }
}

fn str_or_null(raw: &[(String, PyValue)], key: &str) -> Result<Option<String>, BadInput> {
    match field(raw, key) {
        PyValue::Null => Ok(None),
        PyValue::Str(s) => Ok(Some(s.clone())),
        other => Err(BadInput(format!(
            "{key} must be a string or null; got {}",
            repr(other)
        ))),
    }
}

fn issue(v: &PyValue) -> Option<IssueClaim> {
    let PyValue::Dict(d) = v else { return None };
    let n = match field(d, "n") {
        PyValue::Int(i) => i.clone(),
        _ => return None,
    };
    let state = lower_py(&py_str(field(d, "state")));
    if state != "open" && state != "closed" {
        return None;
    }
    let reason = match d.iter().find(|(k, _)| k == "reason") {
        None => String::new(),
        Some((_, PyValue::Str(s))) => s.clone(),
        Some(_) => return None,
    };
    Some(IssueClaim {
        n,
        state: py_str(field(d, "state")),
        reason,
    })
}

/// `parse_event(raw)`: the validated event.
///
/// Why: the one gate between a hook-written event and the commands it drives.
/// What: the script's checks in its order: an object, `kind`, `pr` and
/// `ci_run` (an integer, a digit string or null; a bool is refused), the
/// string fields, `claims` (an object or null), `claims.files|commits`,
/// `claims.checks` and `claims.issues`.
/// Test: the `badinput-*` recordings.
///
/// # Errors
/// [`BadInput`] with the script's message.
pub(crate) fn parse_event(raw: &PyValue) -> Result<Event, BadInput> {
    let PyValue::Dict(raw) = raw else {
        return Err(BadInput("event must be a JSON object".into()));
    };
    let kind = match field(raw, "kind") {
        PyValue::Str(k) if KINDS.contains(&k.as_str()) => k.clone(),
        other => {
            return Err(BadInput(format!(
                "kind must be one of {}; got {}",
                KINDS.join(", "),
                repr(other)
            )));
        }
    };
    let pr = int_or_null(raw, "pr")?;
    let ci_run = int_or_null(raw, "ci_run")?;
    let mut strings = Vec::new();
    for k in [
        "repo",
        "branch",
        "head",
        "base",
        "merge_sha",
        "crate",
        "version",
    ] {
        strings.push(str_or_null(raw, k)?);
    }
    let claims = match field(raw, "claims") {
        PyValue::Null => PyValue::Dict(Vec::new()),
        d @ PyValue::Dict(_) => d.clone(),
        _ => return Err(BadInput("claims must be an object or null".into())),
    };
    let PyValue::Dict(cl) = &claims else {
        return Err(BadInput("claims must be an object or null".into()));
    };
    for k in ["files", "commits"] {
        let v = field(cl, k);
        if !matches!(v, PyValue::Null) && !is_plain_int(v) {
            return Err(BadInput(format!(
                "claims.{k} must be an integer; got {}",
                repr(v)
            )));
        }
    }
    match field(cl, "checks") {
        PyValue::Null => {}
        PyValue::Dict(chk)
            if chk.iter().all(|(k, v)| {
                matches!(k.as_str(), "pass" | "fail" | "pending") && is_plain_int(v)
            }) => {}
        _ => {
            return Err(BadInput(
                "claims.checks must be {pass,fail,pending: integer}".into(),
            ));
        }
    }
    let issues = match field(cl, "issues") {
        PyValue::Null => Vec::new(),
        PyValue::List(items) => items.iter().map(issue).collect::<Option<Vec<_>>>().ok_or_else(
            || {
                BadInput(
                    "claims.issues must be a list of {n: integer, state: open|closed, reason?: string}"
                        .into(),
                )
            },
        )?,
        _ => {
            return Err(BadInput(
                "claims.issues must be a list of {n: integer, state: open|closed, reason?: string}"
                    .into(),
            ));
        }
    };
    let mut it = strings.into_iter();
    let mut next = || it.next().flatten();
    let repo = next();
    let _branch = next();
    Ok(Event {
        kind,
        repo,
        pr,
        head: next(),
        base: next(),
        merge_sha: next(),
        crate_name: next(),
        version: next(),
        ci_run,
        claims,
        issues,
    })
}

/// Why `load_event` produced no event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFailure {
    /// `BadInput`: printed as `BADINPUT <msg>`, exit 2.
    Bad(BadInput),
    /// An uncaught exception (an unreadable file): its traceback's last line.
    Raised(String),
}

/// `load_event(arg, stdin)`: the event from a file path, `-` or literal JSON.
///
/// # Errors
/// [`LoadFailure`] as the script raises.
pub fn load_event(arg: &str, stdin: &mut dyn Read) -> Result<Event, LoadFailure> {
    let text = if arg == "-" {
        let mut buf = Vec::new();
        // A failed read leaves what arrived; the decode below decides.
        let _ = stdin.read_to_end(&mut buf);
        universal_newlines(String::from_utf8_lossy(&buf).into_owned())
    } else if lstrip(arg).starts_with('{') {
        arg.to_owned()
    } else if Path::new(arg).is_file() {
        read_text(Path::new(arg)).map_err(|e| LoadFailure::Raised(e.last_line(arg)))?
    } else {
        let head: String = arg.chars().take(60).collect();
        return Err(LoadFailure::Bad(BadInput(format!(
            "not a file, '-' or JSON object: {}",
            py_repr(&head)
        ))));
    };
    let value = loads(&text)
        .map_err(|e| LoadFailure::Bad(BadInput(format!("invalid JSON: {}", e.py_str()))))?;
    parse_event(&value).map_err(LoadFailure::Bad)
}

/// `_git_sub(argv)`: the git verb after any `-C <dir>` pairs.
fn git_sub(argv: &[String]) -> &str {
    let mut i = 1;
    while argv.get(i).is_some_and(|a| a == "-C") {
        i += 2;
    }
    argv.get(i).map_or("", String::as_str)
}

/// `tool_is_network(argv)`.
pub(crate) fn tool_is_network(argv: &[String]) -> bool {
    matches!(argv.first().map(String::as_str), Some("gh" | "curl"))
}

fn seg_ok(s: &str, extra: &[char]) -> bool {
    !s.is_empty() && s.chars().all(|c| is_word(c) || extra.contains(&c))
}

/// `REST_PATH.fullmatch(path)`:
/// `repos/[\w.-]+/[\w.-]+/(pulls|issues|commits)/[\w./-]+(\?per_page=\d+)?`.
pub(crate) fn rest_path_ok(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("repos/") else {
        return false;
    };
    let mut parts = rest.splitn(4, '/');
    let (Some(owner), Some(name), Some(kind), Some(tail)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if !seg_ok(owner, &['.', '-'])
        || !seg_ok(name, &['.', '-'])
        || !matches!(kind, "pulls" | "issues" | "commits")
    {
        return false;
    }
    let (p, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    let query_ok = query.is_none_or(|q| {
        q.strip_prefix("per_page=")
            .is_some_and(|d| !d.is_empty() && d.chars().all(is_digit))
    });
    seg_ok(p, &['.', '/', '-']) && query_ok
}

/// `guard_read_only(argv)`: `Err` is the `PermissionError` the script raises.
pub(crate) fn guard_read_only(argv: &[String]) -> Result<(), PyExc> {
    let a = |i: usize| argv.get(i).map_or("", String::as_str);
    let ok = match a(0) {
        "gh" => {
            argv.len() >= 3
                && matches!(
                    (a(1), a(2)),
                    ("pr", "view") | ("pr", "checks") | ("run", "view")
                )
                || (argv.len() == 3 && a(1) == "api" && rest_path_ok(a(2)))
        }
        "git" => matches!(
            git_sub(argv),
            "rev-parse" | "merge-base" | "rev-list" | "diff"
        ),
        "curl" => {
            let bad = [
                "-X",
                "-d",
                "--data",
                "-T",
                "-o",
                "-O",
                "--request",
                "--upload-file",
            ];
            argv.last()
                .is_some_and(|l| l.starts_with(&format!("{CRATES_API}/")))
                && !argv.iter().any(|x| bad.contains(&x.as_str()))
        }
        _ => false,
    };
    if ok {
        return Ok(());
    }
    let shown: Vec<String> = argv.iter().take(3).map(|s| py_repr(s)).collect();
    Err(PyExc(format!(
        "PermissionError: verify-claim refuses non-read-only command: [{}]",
        shown.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_path_follows_the_script_pattern() {
        assert!(rest_path_ok("repos/example-org/example-repo/issues/5"));
        assert!(rest_path_ok(
            "repos/a.b/c-d/commits/abc/check-runs?per_page=100"
        ));
        assert!(!rest_path_ok("repos/example-org/example repo/issues/5"));
        assert!(!rest_path_ok("repos/a/b/labels/5"));
        assert!(!rest_path_ok("repos/a/b/pulls/"));
        assert!(!rest_path_ok("repos/a/b/pulls/1?per_page="));
        assert!(!rest_path_ok("repos/a/b/pulls/1?page=2"));
        let argv: Vec<String> = ["gh", "pr", "merge", "1"].map(String::from).to_vec();
        assert_eq!(
            guard_read_only(&argv),
            Err(PyExc(
                "PermissionError: verify-claim refuses non-read-only command: ['gh', 'pr', 'merge']"
                    .into()
            ))
        );
    }
}
