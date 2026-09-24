//! Which `git` invocations a read-only dispatch may run (#8439).
//!
//! Why: the first cut of #8439 allowed `git checkout <x>` when `<x>` did not
//! exist on disk, judged on unexpanded text, and walked unknown global options
//! as valueless — so `git --attr-source status checkout -- Cargo.toml` and
//! `git checkout ':/Cargo.toml'` both restored files. This module names the
//! reads and refuses everything else, with no filesystem check.
//! What: [`check_git`] allows the global options in [`GLOBAL_FLAGS`] and
//! `-C <literal>`, then one subcommand:
//! - `status`, `log`, `diff`, `show`, `grep`, `rev-parse`, `ls-files`,
//!   `merge-base`, `ls-remote`, with no option that writes a file or runs a
//!   program (`--output`, `-O`/`--open-files-in-pager`, `-u`/`--upload-pack`,
//!   `--exec`, `--ext-diff`, `--textconv`, `--show-signature`, a `%G…`
//!   format placeholder with or without a `+`/`-`/space modifier, a branch
//!   format naming `signature`, and any
//!   abbreviation git would expand to one of them). Every literal is checked,
//!   including those after `--`: git reads `--` as the VALUE of a preceding
//!   `-e`, so `git grep -e -- -O` still parses `-O` (#8439 round 2). A
//!   pathspec spelled like a refused option is refused too. `ls-remote` also
//!   refuses a `<transport>::` remote-helper URL. A `for` variable is refused
//!   as an `ls-remote` argument and straight after any option but `--`, where
//!   it could be that option's value (#8439 round 3);
//! - `branch` with listing options only, each value a literal, and a pattern
//!   only after `--list`/`-l`;
//! - `worktree list [--porcelain|-v|--verbose|-z]`.
//!
//! `checkout`, `restore`, `switch`, `fetch` and every other subcommand are
//! refused: each moves a ref or rewrites the tree, and `git checkout <name>`
//! cannot be told from `git checkout <path>` without asking the filesystem.
//! Out of scope: a program run by configuration that already exists —
//! `core.fsmonitor`, `diff.external`, a textconv driver, `gpg.program`,
//! `core.pager` — and `git status` refreshing `.git/index`.
//! Test: `read_only_allow_tests::git_reads_pass_and_everything_else_is_refused`.

use super::read_only_programs::Arg;

type Verdict = Result<(), String>;

/// Valueless git global options a read may carry.
const GLOBAL_FLAGS: &[&str] = &[
    "--no-pager",
    "-P",
    "--no-optional-locks",
    "--literal-pathspecs",
];

/// Subcommands that only read the repository and print.
const READS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "grep",
    "rev-parse",
    "ls-files",
    "merge-base",
    "ls-remote",
];

/// Long options, by full name, that write a file or run a program.
///
/// What: git's parse-options accepts any unambiguous prefix, so a long option
/// is refused when its name is a prefix of one of these (`--out` → `--output`).
const WRITING_LONG: &[&str] = &[
    "output",
    "open-files-in-pager",
    "upload-pack",
    "exec",
    "ext-diff",
    "textconv",
    "show-signature",
];

/// `git branch` options that only list.
const BRANCH_FLAGS: &[&str] = &[
    "--list",
    "-l",
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-v",
    "-vv",
    "--verbose",
    "--show-current",
    "--no-color",
    "--no-column",
    "-i",
    "--ignore-case",
];

/// `git branch` listing options that take one value.
const BRANCH_VALUED: &[&str] = &[
    "--contains",
    "--no-contains",
    "--merged",
    "--no-merged",
    "--points-at",
    "--sort",
    "--format",
];

/// Judge a `git` argv (`rest` excludes `git` itself).
///
/// Why: see the module doc.
/// What: `Ok` for an allowlisted read; `Err` naming the refusal otherwise.
/// Test: as the module doc.
pub(super) fn check_git(rest: &[Arg]) -> Verdict {
    let mut i = 0;
    let sub = loop {
        let Some(arg) = rest.get(i) else {
            return Err("`git` with no subcommand".into());
        };
        let t = arg
            .text()
            .ok_or("a `git` subcommand that is not a literal")?;
        if GLOBAL_FLAGS.contains(&t) {
            i += 1;
        } else if t == "-C" {
            if !rest
                .get(i + 1)
                .is_some_and(|a| a.text().is_some() && a.is_operand())
            {
                return Err("`git -C` without a literal directory".into());
            }
            i += 2;
        } else if t.starts_with('-') {
            return Err(format!(
                "the git global option `{t}`, which is not allowlisted"
            ));
        } else {
            break t;
        }
    };
    let tail = &rest[i + 1..];
    match sub {
        s if READS.contains(&s) => read(s, tail),
        "branch" => branch(tail),
        "worktree" => worktree(tail),
        _ => Err(format!(
            "`git {sub}`, which is not a read (a read-only agent never moves a ref or restores a file)"
        )),
    }
}

/// A read subcommand without an option that writes or runs a program.
fn read(sub: &str, tail: &[Arg]) -> Verdict {
    // #8439 round 3: a `for` variable is judged by where it stands, since its
    // text is unknown — never an `ls-remote` remote (a `::` helper URL), and
    // never straight after an option, whose value it could be (`--format`).
    for (k, arg) in tail.iter().enumerate() {
        if arg.text().is_some() {
            continue;
        }
        let after_option = k
            .checked_sub(1)
            .and_then(|p| tail[p].text())
            .is_some_and(|prev| prev.starts_with('-') && prev != "--");
        if sub == "ls-remote" || after_option {
            return Err(format!(
                "a `for` variable as the value of a `git {sub}` option or remote (to pass a \
                 loop path to git, put it after `--`: `git {sub} <options> -- \"$f\"`)"
            ));
        }
    }
    // #8439 round 2: no stop at `--` — it may be an option's value.
    for t in tail.iter().filter_map(Arg::text) {
        if sub == "ls-remote" && t.contains("::") {
            return Err("a `git ls-remote` remote-helper URL, which runs a program".into());
        }
        // #8439 round 2 audit: a `%G…` pretty-format placeholder verifies the
        // commit signature, which runs `gpg.program` like `--show-signature`.
        // Round 3 critic: git takes one `+`, `-` or space between `%` and the
        // placeholder (`%-G?`, `% G?`, `%+GS`); more are refused too.
        if signature_placeholder(t) {
            return Err("a `%G` format placeholder, which runs the signature verifier".into());
        }
        let long = t
            .strip_prefix("--")
            .map(|n| n.split('=').next().unwrap_or(n));
        let writes_long =
            long.is_some_and(|n| !n.is_empty() && WRITING_LONG.iter().any(|w| w.starts_with(n)));
        let cluster = t.strip_prefix('-').filter(|c| !c.starts_with('-'));
        let writes_short = cluster.is_some_and(|c| {
            (sub == "grep" && c.contains('O')) || (sub == "ls-remote" && c.contains('u'))
        });
        if writes_long || writes_short {
            return Err(format!(
                "`git {sub} {t}`, which writes a file or runs a program"
            ));
        }
    }
    Ok(())
}

/// `git branch` that only lists.
fn branch(tail: &[Arg]) -> Verdict {
    let listing = tail
        .iter()
        .any(|a| matches!(a.text(), Some("--list" | "-l")));
    let mut i = 0;
    // #8439 round 2 audit: `%(signature…)` in `--format` runs the verifier;
    // round 3: `%(*signature)` too, so any mention of `signature` is refused.
    if tail
        .iter()
        .filter_map(Arg::text)
        .any(|t| t.contains("signature"))
    {
        return Err("a `%(signature)` format atom, which runs the signature verifier".into());
    }
    while let Some(arg) = tail.get(i) {
        let t = arg.text().unwrap_or_default();
        let name = t.split('=').next().unwrap_or(t);
        let joined = t.contains('=') && BRANCH_VALUED.contains(&name);
        if BRANCH_VALUED.contains(&t) {
            // #8439 round 3: the value's content matters (`%(signature)`).
            if tail.get(i + 1).and_then(Arg::text).is_none() {
                return Err(format!("`git branch {t}` without a literal value"));
            }
            i += 2;
        } else if BRANCH_FLAGS.contains(&t) || joined || (listing && arg.is_operand()) {
            i += 1;
        } else {
            return Err("`git branch` in a form that creates, moves or deletes a branch".into());
        }
    }
    Ok(())
}

/// `git worktree list` with listing options only.
fn worktree(tail: &[Arg]) -> Verdict {
    let listed = tail.first().and_then(Arg::text) == Some("list");
    let flags_ok = tail
        .iter()
        .skip(1)
        .all(|a| matches!(a.text(), Some("--porcelain" | "-v" | "--verbose" | "-z")));
    if !listed || !flags_ok {
        return Err("`git worktree` in a form other than `list`".into());
    }
    Ok(())
}

/// Does `t` hold a `%G…` pretty-format placeholder (#8439)?
///
/// Why: `%G?`, `%GS`, `%GK` and the rest verify the commit signature, which
/// runs `gpg.program`. Git accepts one `+`, `-` or space after the `%`.
/// What: true when some `%` is followed, after any run of `+`/`-`/space, by
/// `G`. Over-refuses `%%G`, a literal `%` before `G`.
/// Test: `read_only_allow_tests::git_options_after_a_double_dash_are_still_judged`.
fn signature_placeholder(t: &str) -> bool {
    t.match_indices('%').any(|(k, _)| {
        t[k + 1..]
            .trim_start_matches(['+', '-', ' '])
            .starts_with('G')
    })
}
