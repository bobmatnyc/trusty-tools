//! Every verb that can move a main checkout's HEAD is gated on the
//! `documents_only` declaration not changing (#7905 review round 2).
//!
//! Why: the first two cuts of #7905 closed the two routes that pass the
//! ADR-0049 commit gate — the working-tree read, and `git add .trusty-mpm.toml
//! && git commit` — and left every route that does not. `git merge
//! other/declare-branch` was ALLOWED on the built binary in a solo session,
//! after which `git show HEAD:.trusty-mpm.toml` read `documents_only = true`
//! and a `Write src/lib.rs` in that checkout was admitted. The ADR-0048
//! decision 10 head-move rule next door did not stop it: it covers `merge` and
//! `rebase` only, and denies only when the DAEMON reports another live writer,
//! so a solo session or an unreachable daemon always allowed. `cherry-pick`,
//! `checkout -B`, `reset`, `update-ref`, `symbolic-ref`, `am` and `apply
//! --index` matched no gate at all.
//!
//! What: [`evaluate_declaration_head_move`] denies, with no daemon round-trip
//! and independently of who else is standing in the tree, when a HEAD-moving
//! git verb aimed at a main checkout would land a DIFFERENT `documents_only`
//! value than the one `HEAD` carries now. It is a strictly separate rule from
//! ADR-0048 decision 10, which stays exactly as it was: that one asks whether
//! moving HEAD would disturb another session, this one asks whether the move
//! changes the declaration. Both run.
//!
//! **It proves "unchanged", rather than detecting "changed".** Every candidate
//! operand is put to `git rev-parse --verify <tok>^{commit}`, so no flag table
//! has to be kept in step with git's: a `--strategy=ours` or a `-m` resolves to
//! nothing and is ignored, while a branch, a tag and a `HEAD~1` all resolve and
//! are compared. An operand that is named but does NOT resolve is a revision
//! the guard cannot vouch for, and denies. A verb that names no operand at all
//! lands on `HEAD` or on `@{upstream}`, both of which are read the same way.
//!
//! **A replay verb is answered by a narrower question.** `cherry-pick` and
//! `revert` do not land a target tree, so the resulting declaration is not
//! knowable in advance; what IS knowable is whether the commits being replayed
//! touch `.trusty-mpm.toml` at all
//! ([`rev_touches_declaration`](trusty_mpm::core::project_config::rev_touches_declaration)).
//! If they do not, the declaration cannot move.
//!
//! **`am` and `apply --index` name no revision**, so neither question can be
//! asked of them, and they deny in a main checkout. That is a deliberate
//! widening: both were already listed as residual bypasses by
//! `pm_guard_write_boundary` — they write the files a patch names, and the
//! patch names them, not the argv — and #7905 turns that bypass into one that
//! can land the declaration itself. The remedy the message names is the same as
//! every other ADR-0044 refusal: do it in a worktree.
//!
//! Verbs that cannot move HEAD stay ungated and are not listed here at all:
//! `fetch`, `log`, `show`, `status`, `diff`, and a `stash` that is not popping.
//! Test: `declaration_head_move_tests`.

use std::path::Path;

use trusty_mpm::core::project_aliases::main_checkout_root;
use trusty_mpm::core::project_config::{
    declared_documents_only_at_rev, rev_is_resolvable, rev_touches_declaration,
};

use super::PathEnv;
use super::main_checkout::git_verb_target_dir_with_tail;

/// How a verb would put a new commit under `HEAD`.
///
/// Why: three shapes need three questions, and naming them is what keeps the
/// policy below readable as a policy rather than as a parser.
/// What: `Lands` — HEAD ends up AT a nameable revision, so the two declarations
/// are compared directly. `Replays` — a diff is applied onto the current tree,
/// so the question is whether that diff touches the declaration. `Unknowable` —
/// no revision is named at all, which can only fail closed.
/// Test: `declaration_head_move_tests`.
#[derive(Debug, PartialEq, Eq)]
enum HeadMove {
    /// HEAD lands at these revisions.
    Lands(Vec<String>),
    /// These revisions' diffs are replayed onto the current tree.
    Replays(Vec<String>),
    /// Nothing nameable — `am`, `apply --index`.
    Unknowable,
}

/// Deny a HEAD move that would change a main checkout's declaration (#7905).
///
/// Why: the one entry point `pm_guard` calls, kept to the wrapper shape its
/// sibling rules use so the process-environment read happens in exactly one
/// place and the policy underneath stays testable without it.
/// What: `Some(reason)` when a HEAD-moving verb aimed at a main checkout would
/// land a different `documents_only` value than `HEAD` carries; `None` (ALLOW)
/// otherwise, including for every verb that cannot move HEAD and for every
/// directory that is not a main checkout.
/// Test: `a_merge_of_a_declaring_ref_is_denied`,
/// `a_merge_of_a_matching_ref_is_allowed`,
/// `a_cherry_pick_of_a_declaring_commit_is_denied`,
/// `a_checkout_b_to_a_declaring_ref_is_denied`,
/// `a_reset_soft_to_a_declaring_ref_is_denied`,
/// `an_update_ref_of_the_current_branch_is_denied`,
/// `a_fetch_of_a_declaring_branch_is_allowed`.
pub(crate) fn evaluate_declaration_head_move(command: &str, cwd: &Path) -> Option<String> {
    evaluate_declaration_head_move_in(command, cwd, &PathEnv::from_process())
}

/// [`evaluate_declaration_head_move`] with the process environment injected.
///
/// Why: the same seam every sibling rule carries — mutating `$HOME` in-process
/// to test a path races every other test in the binary.
/// Test: as [`evaluate_declaration_head_move`].
fn evaluate_declaration_head_move_in(command: &str, cwd: &Path, env: &PathEnv) -> Option<String> {
    let (verb, target, tail) = git_verb_target_dir_with_tail(command, cwd, env, |verb, tail| {
        classify_head_move(verb, tail).is_some()
    })?;
    let root = main_checkout_root(&target)?;
    let current = declared_documents_only_at_rev(&root, "HEAD");
    match classify_head_move(&verb, &tail)? {
        HeadMove::Unknowable => Some(deny_reason(&verb, &root)),
        HeadMove::Replays(revs) => revs
            .iter()
            .any(|rev| rev_touches_declaration(&root, rev))
            .then(|| deny_reason(&verb, &root)),
        HeadMove::Lands(revs) => {
            let changes = revs.iter().any(|rev| {
                // A named operand that does not resolve is a revision this
                // guard cannot vouch for, which is not the same as one that
                // carries no declaration.
                !rev_is_resolvable(&root, rev)
                    || declared_documents_only_at_rev(&root, rev) != current
            });
            changes.then(|| deny_reason(&verb, &root))
        }
    }
}

/// Flags that continue or abandon an operation already under way.
///
/// Why: same reason ADR-0048 decision 10 carries the list — a deny on these
/// strands a session mid-rebase with no way out, and the move they finish was
/// already judged when it started.
const IN_PROGRESS_FLAGS: &[&str] = &[
    "--abort",
    "--quit",
    "--continue",
    "--skip",
    "--edit-todo",
    "--show-current-patch",
];

/// The ref every verb falls back to when it names no operand.
///
/// Why: `git merge` and `git rebase` with no argument use the branch's upstream,
/// which is a commit someone else pushed and therefore exactly the route #7905's
/// review round 2 found open.
const UPSTREAM: &str = "@{upstream}";

/// Which HEAD-moving shape, if any, this `(verb, argv-tail)` is.
///
/// Why: the whole verb table in one place, so "can this move HEAD" is asked and
/// answered once rather than being spread through the policy above.
/// What: `None` for every verb that cannot move HEAD, and for an in-progress
/// control flag. Operands are the tail tokens BEFORE a `--` separator that do
/// not start with `-`; everything else is left to `git rev-parse` to reject.
/// Test: `declaration_head_move_tests`.
fn classify_head_move(verb: &str, tail: &[String]) -> Option<HeadMove> {
    if tail.iter().any(|t| IN_PROGRESS_FLAGS.contains(&t.as_str())) {
        return None;
    }
    let operands: Vec<String> = tail
        .iter()
        .take_while(|t| t.as_str() != "--")
        .filter(|t| !t.starts_with('-'))
        .cloned()
        .collect();
    match verb {
        // HEAD lands at the named ref, or at the upstream when none is named.
        "merge" | "rebase" => Some(HeadMove::Lands(if operands.is_empty() {
            vec![UPSTREAM.to_string()]
        } else {
            operands
        })),
        // `git reset` with no ref is a reset to HEAD: nothing moves.
        "reset" | "checkout" | "switch" => {
            // `checkout`/`switch` reach here only through a branch-creating
            // flag; a bare `git checkout <x>` is the pathspec-vs-ref ambiguity
            // ADR-0048 decision 10 deliberately leaves alone, and its first
            // operand is the new BRANCH name rather than a start point.
            let mut operands = operands;
            if verb != "reset" {
                if !creates_a_branch(tail) {
                    return None;
                }
                if operands.is_empty() {
                    return None;
                }
                operands.remove(0);
            }
            (!operands.is_empty()).then(|| HeadMove::Lands(operands))
        }
        // The first operand is the ref being WRITTEN, not a revision to read.
        "update-ref" => {
            let (name, values) = operands.split_first()?;
            let moves_head = name == "HEAD" || name.starts_with("refs/heads/");
            (moves_head && !values.is_empty()).then(|| HeadMove::Lands(values.to_vec()))
        }
        "symbolic-ref" => {
            let (name, values) = operands.split_first()?;
            (name == "HEAD" && !values.is_empty()).then(|| HeadMove::Lands(values.to_vec()))
        }
        // A diff replayed onto the current tree.
        "cherry-pick" | "revert" => (!operands.is_empty()).then(|| HeadMove::Replays(operands)),
        // No revision is named at all.
        "am" => Some(HeadMove::Unknowable),
        "apply" => tail
            .iter()
            .any(|t| t == "--index" || t == "--cached")
            .then_some(HeadMove::Unknowable),
        _ => None,
    }
}

/// Does this `checkout`/`switch` tail create or reset a branch at a start point?
///
/// Why: only the branch-creating forms name an unambiguous start point.
/// `-b`/`-B`/`-C`/`--orphan` each take the new branch name and then, optionally,
/// the revision to start it at — which is the revision HEAD lands on.
/// Test: `a_checkout_b_to_a_declaring_ref_is_denied`.
fn creates_a_branch(tail: &[String]) -> bool {
    tail.iter()
        .any(|t| matches!(t.as_str(), "-b" | "-B" | "-C" | "--orphan"))
}

/// Build the refusal.
///
/// Why: the same shape every ADR-0044 deny in this crate uses — name what was
/// blocked, why this directory differs from every other, and the remedy that
/// exists. The declaration is what makes a main checkout writable, so a command
/// that changes it is the highest-consequence change the tree can receive.
/// Test: `a_merge_of_a_declaring_ref_is_denied`.
fn deny_reason(verb: &str, root: &Path) -> String {
    format!(
        "HEAD move denied in a main checkout (ADR-0044 decision 7): `git {verb}` would change \
         the `documents_only` declaration of `{}`. That declaration decides whether this shared \
         checkout accepts source writes at all, so it may only move through a reviewed pull \
         request — the same route as any other source change, and the only route that puts a \
         second pair of eyes on it. Landing it by moving HEAD here would switch the ADR-0044 \
         write boundary on or off for every session standing in this directory, with no diff \
         anyone reviewed. Do the change on a branch in a worktree and open a pull request. \
         `git fetch`, `git log`, `git show`, `git status` and `git diff` are unaffected: none \
         of them moves HEAD.",
        root.display()
    )
}

#[cfg(test)]
#[path = "declaration_head_move_tests.rs"]
mod declaration_head_move_tests;
