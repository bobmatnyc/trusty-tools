//! Main checkouts the operator lists in `~/.trusty-mpm/config.toml`
//! `[pm_guard]` for a narrower rule (#8524, #7905).
//!
//! Why: the main-checkout rules treat every checkout alike, and two kinds of
//! checkout cannot live with that. A cron host's runtime checkout is read in
//! place by launchd jobs, so a worktree cannot move the ref they read (#8524).
//! A documents repository whose own CLAUDE.md forbids worktrees can never
//! commit a utility script by either route (#7905). Both lists live in the
//! operator config, a trust anchor no agent may write, so a local HEAD move or
//! a file an agent writes cannot grant either one — the gap the in-repo
//! declaration on `wip/7905-documents-only-declaration` never closed.
//! What: [`listed`] matches a checkout root against a list by canonical path.
//! [`reset_keep_is_exempt`] lifts the destructive rule for exactly one shape,
//! owner ruling 2026-09-28 (Option C): a lone `git reset --keep [<rev>]` in a
//! `runtime_checkouts` entry whose tracked content already equals `<rev>`.
//! [`is_documents_repo`] reports a `documents_repos` entry. Every unreadable
//! step answers "not listed" or "not exempt", so the rules fail closed.
//! Test: the `#[cfg(test)]` suite below; `pm_guard_allows_reset_keep_in_a_listed_runtime_checkout`
//! in `tests/tm_hook_pm_guard_operator_checkouts.rs` runs the binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::project_aliases::main_checkout_root;

use super::shell_lex;
use super::split_shell_segments;
use crate::commands::hook_rewrite::first_command_token;

/// Whether `root` is one of `list`, compared by canonical path.
///
/// What: `false` when `root` cannot be canonicalized; a list entry that cannot
/// be canonicalized matches nothing.
fn listed(root: &Path, list: &[PathBuf]) -> bool {
    let Ok(real) = root.canonicalize() else {
        return false;
    };
    list.iter()
        .any(|entry| entry.canonicalize().is_ok_and(|e| e == real))
}

/// Whether the main checkout `root` is an operator-listed documents repository
/// (#7905), in which every staged or written path counts as a document.
///
/// Test: `listed_matches_by_canonical_path_only`.
pub(crate) fn is_documents_repo(root: &Path) -> bool {
    listed(root, &MpmConfig::load_default().pm_guard.documents_repos)
}

/// Lift the destructive rule for one `git reset --keep` segment (#8524).
///
/// Why: see the module doc. What: [`reset_keep_exempt_with`] over the operator
/// config and a real `git diff --quiet` content probe.
/// Test: `reset_keep_exempt_*` below.
pub(super) fn reset_keep_is_exempt(
    command: &str,
    verb: &str,
    tail: &[String],
    target: &Path,
) -> bool {
    reset_keep_exempt_with(
        (command, verb, tail, target),
        || MpmConfig::load_default().pm_guard.runtime_checkouts,
        content_matches,
    )
}

/// The policy with the allowlist and the content probe injected.
///
/// What: `true` only when all hold, cheapest first — the verb is `reset`; the
/// tail is `--keep` with at most `-q` and one revision ([`keep_revision`]);
/// the command is that one reset with nothing but `cd` around it; the target's
/// checkout root is in `list`; and `probe(root, rev)` answers `Some(true)`.
/// `None` from the probe (git could not answer) is not exempt.
fn reset_keep_exempt_with(
    (command, verb, tail, target): (&str, &str, &[String], &Path),
    list: impl FnOnce() -> Vec<PathBuf>,
    probe: impl FnOnce(&Path, &str) -> Option<bool>,
) -> bool {
    if verb != "reset" {
        return false;
    }
    let Some(rev) = keep_revision(tail) else {
        return false;
    };
    if !command_is_a_lone_reset(command) {
        return false;
    }
    let Some(root) = main_checkout_root(target) else {
        return false;
    };
    // #8524: no blanket bypass — only allowlisted runtime checkouts.
    listed(&root, &list()) && probe(&root, rev) == Some(true)
}

/// The revision of a `reset --keep` tail, `HEAD` when none is named.
///
/// What: `None` unless the tail holds exactly one `--keep`, any number of
/// `-q`/`--quiet`, and at most one token not led by `-`. Every other flag —
/// `--hard`, `--merge`, `--`, a pathspec — refuses the exemption.
fn keep_revision(tail: &[String]) -> Option<&str> {
    let mut keep = 0;
    let mut rev = None;
    for token in tail {
        match token.as_str() {
            "--keep" => keep += 1,
            "-q" | "--quiet" => {}
            t if !t.starts_with('-') && rev.is_none() => rev = Some(t),
            _ => return None,
        }
    }
    (keep == 1).then_some(rev.unwrap_or("HEAD"))
}

/// Whether `command` is one `git reset` with only `cd` segments around it.
///
/// Why: the content probe runs before the command does, so anything chained
/// could change the tree between the probe and the reset.
fn command_is_a_lone_reset(command: &str) -> bool {
    let mut resets = 0;
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() || first_command_token(trimmed).as_deref() == Some("cd") {
            continue;
        }
        if shell_lex::git_subcommand(trimmed).as_deref() != Some("reset") {
            return false;
        }
        resets += 1;
    }
    resets == 1
}

/// Whether the tracked content of `root`'s working tree equals `rev`.
///
/// What: `git -C <root> diff --quiet --no-ext-diff --no-textconv <rev> --`.
/// Exit 0 is `Some(true)`, exit 1 is `Some(false)`, and anything else — git
/// missing, a bad revision, a signal — is `None` (fail closed).
fn content_matches(root: &Path, rev: &str) -> Option<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            rev,
            "--",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[path = "operator_checkouts_tests.rs"]
mod tests;
