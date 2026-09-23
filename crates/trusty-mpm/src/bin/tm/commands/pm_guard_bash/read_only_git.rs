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
//!   `--exec`, and any abbreviation git would expand to one of them);
//! - `branch` with listing options only, and a pattern only after
//!   `--list`/`-l`;
//! - `worktree list [--porcelain|-v|--verbose|-z]`.
//!
//! `checkout`, `restore`, `switch`, `fetch` and every other subcommand are
//! refused: each moves a ref or rewrites the tree, and `git checkout <name>`
//! cannot be told from `git checkout <path>` without asking the filesystem.
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
const WRITING_LONG: &[&str] = &["output", "open-files-in-pager", "upload-pack", "exec"];

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
    for t in tail.iter().filter_map(Arg::text) {
        if t == "--" {
            break;
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
    while let Some(arg) = tail.get(i) {
        let t = arg.text().unwrap_or_default();
        let name = t.split('=').next().unwrap_or(t);
        let joined = t.contains('=') && BRANCH_VALUED.contains(&name);
        if BRANCH_VALUED.contains(&t) {
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
