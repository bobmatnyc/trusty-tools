//! `tm hook --pm-guard` — the one narrowed exception to the #8625 D5b text
//! rule: `trusty-memory import kuzu` in its dry-run, export-refusals and
//! review forms (#9794).
//!
//! Why: the D5b rule denies every Bash call naming both `trusty-memory` and
//! `kuzu`, so the `tm-memory-import` skill had no sanctioned path. Architect
//! answers (2026-10-12): Q1 any `--python` denies under its own slug; Q2 the
//! allow binds every caller, like the deny; Q3 the `review` subcommands
//! allow; Q4 the only tail is `| tee` into the review root or the session
//! scratchpad; Q5 a form that applies the import stays denied.
//! What: [`kuzu_import_exception`] is a positive, whole-command match on the
//! RAW command text. Anything it does not match falls through to the D5b
//! rule. An allow lifts only that rule: the by-name store deny
//! (`pm_guard_kuzu_store::evaluate_command`) still runs on every word.
//! Residual (D5b, mistakes only): with `--python` absent the importer takes
//! its interpreter from the `kuzu-memory` launcher on PATH.
//! Test: `pm_guard_kuzu_store_tests.rs` — `the_exception_cannot_widen`,
//! `allows_the_dry_run_export_and_review_forms`,
//! `denies_any_python_flag_with_its_own_slug`,
//! `the_allow_keeps_the_by_name_store_deny`.

use std::path::Path;

/// The audit label and reason prefix of a `--python` deny (answer Q1).
pub(crate) const KUZU_IMPORT_PYTHON_RULE: &str = "kuzu-import-python";

/// The verdict of the exception on one Bash command.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImportException {
    /// A sanctioned form: skip the D5b text rule, nothing else.
    Allow,
    /// The import form with `--python`: deny with this reason.
    DenyPython(String),
    /// Not the exception: the D5b text rule decides.
    NoMatch,
}

/// The one optional prefix, word for word.
const PREFIX: [&str; 3] = ["env", "-u", "TRUSTY_MEMORY_PALACE"];

/// The program words, exact and adjacent.
const PROGRAM: [&str; 3] = ["trusty-memory", "import", "kuzu"];

/// The optional stderr merge, directly before the tee tail or at the end.
const STDERR_TAIL: &str = " 2>&1";

/// The one allowed pipe: into `tee` with a single path.
const TEE_TAIL: &str = " | tee ";

/// Characters the command body and the tee path may not hold: quoting,
/// expansion, globbing, chaining, redirection, comments and any whitespace
/// but a single space.
const BANNED: &[char] = &[
    '$', '\'', '"', '\\', '`', ';', '&', '|', '<', '>', '(', ')', '{', '}', '[', ']', '*', '?',
    '!', '#', '\n', '\r', '\t',
];

/// Import-form flags that take no value.
const IMPORT_SWITCHES: &[&str] = &["--discover", "--dry-run", "--export-refusals", "--update"];

/// Import-form flags that take the next word as their value.
const IMPORT_VALUED: &[&str] = &[
    "--root",
    "--max-depth",
    "--bridge-timeout-secs",
    "--from",
    "--palace",
    "--review-ledger",
];

/// `review record` flags, each taking a value (skill `tm-memory-import` §3).
const RECORD_VALUED: &[&str] = &["--run", "--key", "--verdict", "--op"];

/// `review status` and `review finish` flags.
const RUN_VALUED: &[&str] = &["--run"];

/// The review root under the home, a tee target (answer Q4).
const REVIEW_ROOT: &str = ".trusty-tools/kuzu-review";

/// File names the review code writes into a run directory (`ledger.rs`,
/// `export.rs`); a tee onto one would truncate the run's sealed verdicts or
/// masked rows (#9794).
const RUN_ARTIFACTS: [&str; 4] = [
    "ledger.jsonl",
    "ledger.seal",
    "masked.jsonl",
    ".masked.jsonl.partial",
];

/// The directories a session scratchpad lives under (`/tmp` on macOS is
/// `/private/tmp`).
const SCRATCH_ROOTS: [&str; 2] = ["/private/tmp/claude-", "/tmp/claude-"];

/// The exception's verdict on a raw Bash `command`; `home` is the user's
/// home, for the absolute spelling of the review root.
///
/// Why: see the module doc; default-deny, so an evasion cannot match it.
/// What: an optional `| tee <path>` tail, then an optional ` 2>&1`, are cut
/// off; the body must hold no [`BANNED`] character and split on single
/// spaces into an optional [`PREFIX`], the [`PROGRAM`] words, and either
/// `review record|status|finish` with its flags or the import flags with
/// `--dry-run` present. Any `--python` word there denies. Every value is one
/// word that does not start with `-`, so no flag can swallow `--dry-run`.
/// Test: `the_exception_cannot_widen`,
/// `allows_the_dry_run_export_and_review_forms`,
/// `denies_any_python_flag_with_its_own_slug`.
pub(crate) fn kuzu_import_exception(command: &str, home: Option<&Path>) -> ImportException {
    let (body, tee) = match command.split_once(TEE_TAIL) {
        Some((body, path)) => (body, Some(path)),
        None => (command, None),
    };
    let body = body.strip_suffix(STDERR_TAIL).unwrap_or(body);
    if body.contains(BANNED) || tee.is_some_and(|path| !tee_path_allowed(path, home)) {
        return ImportException::NoMatch;
    }
    let words: Vec<&str> = body.split(' ').collect();
    let words = words.strip_prefix(PREFIX.as_slice()).unwrap_or(&words);
    let Some(args) = words.strip_prefix(PROGRAM.as_slice()) else {
        return ImportException::NoMatch;
    };
    if args
        .iter()
        .any(|w| *w == "--python" || w.starts_with("--python="))
    {
        return ImportException::DenyPython(python_reason());
    }
    let sanctioned = match args {
        ["review", sub, rest @ ..] => review_form(sub, rest),
        _ => args.contains(&"--dry-run") && flags_match(args, IMPORT_SWITCHES, IMPORT_VALUED),
    };
    if sanctioned {
        ImportException::Allow
    } else {
        ImportException::NoMatch
    }
}

/// Whether `review <sub> <rest>` is a sanctioned review call.
fn review_form(sub: &str, rest: &[&str]) -> bool {
    let valued = match sub {
        "record" => RECORD_VALUED,
        "status" | "finish" => RUN_VALUED,
        _ => return false,
    };
    flags_match(rest, &[], valued)
}

/// Whether every word of `args` is a switch, or a valued flag followed by
/// one value word that does not start with `-`.
fn flags_match(args: &[&str], switches: &[&str], valued: &[&str]) -> bool {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if switches.contains(word) {
            continue;
        }
        if !valued.contains(word) {
            return false;
        }
        if !words
            .next()
            .is_some_and(|value| !value.is_empty() && !value.starts_with('-'))
        {
            return false;
        }
    }
    true
}

/// Whether a tee `path` is a file under the review root or a session
/// scratchpad (answer Q4).
///
/// What: one word with no [`BANNED`] character or space; then
/// `~/.trusty-tools/kuzu-review/<file>` or `<home>/.trusty-tools/kuzu-review/<file>`,
/// or `{/private,}/tmp/claude-<uid digits>/<project>/<session>/scratchpad/<file>`.
/// Every component after the fixed prefix is non-empty and neither `.` nor
/// `..`, and `<file>` has at least one component. A last component named in
/// [`RUN_ARTIFACTS`] is refused.
/// Test: `the_exception_cannot_widen`, `allows_the_dry_run_export_and_review_forms`.
fn tee_path_allowed(path: &str, home: Option<&Path>) -> bool {
    if path.contains(BANNED) || path.contains(' ') {
        return false;
    }
    if path
        .rsplit('/')
        .next()
        .is_some_and(|name| RUN_ARTIFACTS.contains(&name))
    {
        return false;
    }
    if let Some(rest) = path.strip_prefix(format!("~/{REVIEW_ROOT}/").as_str()) {
        return plain_components(rest);
    }
    if let Some(home) = home.and_then(Path::to_str) {
        let root = format!("{}/{REVIEW_ROOT}/", home.trim_end_matches('/'));
        if let Some(rest) = path.strip_prefix(root.as_str()) {
            return plain_components(rest);
        }
    }
    scratchpad_file(path)
}

/// Whether `path` is a file inside a session scratchpad.
fn scratchpad_file(path: &str) -> bool {
    let Some(rest) = SCRATCH_ROOTS
        .iter()
        .find_map(|root| path.strip_prefix(root))
    else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let [uid, project, session, "scratchpad", file @ ..] = parts.as_slice() else {
        return false;
    };
    !uid.is_empty()
        && uid.bytes().all(|b| b.is_ascii_digit())
        && plain(project)
        && plain(session)
        && !file.is_empty()
        && file.iter().all(|c| plain(c))
}

/// Whether every `/`-separated component of `rest` is [`plain`].
fn plain_components(rest: &str) -> bool {
    rest.split('/').all(plain)
}

/// A path component that is non-empty and neither `.` nor `..`.
fn plain(component: &str) -> bool {
    !component.is_empty() && component != "." && component != ".."
}

/// The deny text for an import call carrying `--python` (answer Q1).
fn python_reason() -> String {
    format!(
        "{KUZU_IMPORT_PYTHON_RULE}: `trusty-memory import kuzu` with `--python` is denied under \
         the guard (#9794): the flag runs the program it names as the import's interpreter. \
         Omit `--python`; the operator runs an import that needs it outside the guard."
    )
}
