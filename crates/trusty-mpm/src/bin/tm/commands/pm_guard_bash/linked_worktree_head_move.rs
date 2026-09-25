//! HEAD moves aimed at a LINKED worktree another agent is standing in (#8161).
//!
//! Why: Claude Code always mints a fresh isolation worktree, and the `isolation`
//! field says only whether to isolate, never where. So a dispatch cannot resume
//! work parked in an existing worktree; the agent commits in its own tree and a
//! non-pinned caller (the PM or `version-control`) consolidates the result into
//! the parked one: `git -C <parked> fetch <agent-tree> <branch> && git -C
//! <parked> reset --keep FETCH_HEAD`. Nothing governed that second command.
//! The main-checkout rules stop at `main_checkout_root`, so a `reset --keep` or
//! `merge --ff-only` into a linked worktree was allowed even while a live agent
//! was writing there — moving the branch its uncommitted work sits on.
//!
//! What: [`classify_linked_worktree_head_move`] finds the first segment that
//! moves HEAD — `reset --keep`/`--hard`/`--merge`, or a `merge`/`rebase` that
//! starts rather than resumes — and resolves its target through `cd` and
//! `git -C` with the walker the main-checkout rules share. A target whose
//! [`worktree_root`] is a linked worktree is asked about; the main checkout is
//! left to `main_checkout`. [`linked_worktree_head_move_deny`] then asks the
//! daemon who holds that tree, counting the asking session's own agents
//! ([`TREE_HOLDERS_MARKER`]), and denies on any live writer.
//!
//! **Fail closed.** A daemon that is absent, silent, or unparseable has not said
//! the tree is idle, so the move is denied and the deny names `tm repair
//! delegation`. A target this cannot resolve (`$WT`, `$(…)`, a surviving `~`) is
//! denied too, through the shared `unresolved_target` detector.
//!
//! **A subagent's own tree is exempt, lexically.** An isolated agent resetting
//! or rebasing its own worktree is the recipe's step 2 and ordinary work; the
//! harness pins it there, so a target in the caller's own tree needs no daemon
//! round trip and never fails closed on one. The PM gets no such exemption:
//! its cwd can be moved into an agent's worktree (#8535).
//!
//! Residuals, stated: only the FIRST HEAD-moving segment is classified, as in
//! the main-checkout rule; `git pull` is not classified (ADR-0053); a
//! non-isolated subagent standing in another agent's tree passes the
//! own-tree exemption, which is the #4480 dispatch guard's question to answer.
//!
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::project_aliases::worktree_root;
use trusty_mpm::daemon::delegation_routes::TREE_HOLDERS_MARKER;

use super::main_checkout::{git_verb_target_dir_with_tail, starts_a_head_move};
use super::{PathEnv, unresolved_target};
use crate::commands::pm_guard::{audit_denied_tool, build_pm_guard_deny_response};
use crate::commands::pm_guard_dispatch;
use crate::commands::pm_guard_fanout;

/// A HEAD move whose target is a linked worktree, awaiting the daemon's answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LinkedHeadMove {
    /// The git subcommand (`reset`, `merge`, `rebase`).
    pub(crate) verb: String,
    /// The directory the command resolves to through `cd` and `git -C`.
    pub(crate) target: PathBuf,
    /// The linked worktree `target` sits in.
    pub(crate) root: PathBuf,
}

/// What the lexical half decided.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LinkedHeadMoveCheck {
    /// Refused without asking the daemon — the target cannot be resolved.
    Deny(String),
    /// Ask the daemon who holds [`LinkedHeadMove::root`].
    Query(LinkedHeadMove),
}

/// Whether a git subcommand, given its argv tail, moves HEAD in a way a live
/// agent in the same tree would lose work to.
///
/// Why: `reset --keep` is the consolidation step; `--hard` and `--merge` move
/// HEAD the same way and destroy more, so guarding only `--keep` would push a
/// caller to the worse spelling. `merge` (including `--ff-only`) and `rebase`
/// reuse the main-checkout classifier, so an in-progress `--abort`/`--continue`
/// is never refused.
/// Test: `moves_a_linked_head_covers_the_consolidation_verbs`.
fn moves_a_linked_head(subcommand: &str, tail: &[String]) -> bool {
    match subcommand {
        "reset" => tail
            .iter()
            .any(|t| matches!(t.as_str(), "--keep" | "--hard" | "--merge")),
        _ => starts_a_head_move(subcommand, tail),
    }
}

/// Classify one Bash command against the linked-worktree rule.
///
/// Why: the lexical half, kept pure apart from reading the process environment
/// for `~`/`$HOME`, so the rule's scope is unit-testable with no daemon.
/// What: `None` when no segment moves HEAD, when the target is not in a linked
/// worktree, or when a subagent targets its own tree. `Deny` for an unresolved
/// target. Otherwise `Query`.
/// Test: `classify_*` below.
pub(crate) fn classify_linked_worktree_head_move(
    command: &str,
    cwd: &Path,
    caller_is_subagent: bool,
) -> Option<LinkedHeadMoveCheck> {
    classify_in(command, cwd, caller_is_subagent, &PathEnv::from_process())
}

/// [`classify_linked_worktree_head_move`] against an explicit environment.
fn classify_in(
    command: &str,
    cwd: &Path,
    caller_is_subagent: bool,
    env: &PathEnv,
) -> Option<LinkedHeadMoveCheck> {
    let (verb, target, _) = git_verb_target_dir_with_tail(command, cwd, env, moves_a_linked_head)?;
    // #8161: a target this cannot place is refused, whichever tree it names.
    if let Some(unresolved) = unresolved_target(&target) {
        return Some(LinkedHeadMoveCheck::Deny(unresolved_deny_reason(
            &verb,
            &unresolved.shown,
            &unresolved.token,
        )));
    }
    let root = worktree_root(&target)?;
    // #8161: an agent moving its own tree's HEAD is ordinary work, not a consolidation.
    if caller_is_subagent && worktree_root(cwd).as_deref() == Some(root.as_path()) {
        return None;
    }
    Some(LinkedHeadMoveCheck::Query(LinkedHeadMove {
        verb,
        target,
        root,
    }))
}

/// Decide a [`LinkedHeadMove`] from the daemon's answer.
///
/// Why: kept apart from the network call so every arm — idle, occupied, and
/// unanswered — is pinned by a unit test.
/// What: `None` for an answered empty list; a deny naming the writers for a
/// non-empty one; a fail-closed deny naming the failure for `Err`.
/// Test: `verdict_denies_a_live_writer`, `verdict_allows_an_idle_worktree`,
/// `verdict_fails_closed_when_the_lookup_cannot_answer`.
pub(crate) fn linked_head_move_verdict(
    mv: &LinkedHeadMove,
    live: Result<&[String], &str>,
) -> Option<String> {
    match live {
        Ok([]) => None,
        Ok(names) => Some(live_writer_deny_reason(mv, names)),
        Err(detail) => Some(unanswered_deny_reason(mv, detail)),
    }
}

/// Run the rule for one `Bash` call and print its deny (#8161).
///
/// Why: `pm_guard.rs` sits at the 500-SLOC cap, so its call site is one
/// statement. What: [`linked_worktree_head_move_deny`] for this payload's
/// caller; on a deny, audits it, prints it, and returns `true`.
/// Test: `pm_guard_denies_a_reset_keep_into_a_live_agents_worktree` in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) async fn deny_linked_worktree_head_move(
    url: &str,
    session_id: &str,
    payload: &Value,
    command: &str,
    cwd: &Path,
) -> bool {
    let subagent = pm_guard_fanout::caller_is_subagent(payload);
    let deny = linked_worktree_head_move_deny(url, session_id, command, cwd, payload, subagent);
    let Some(reason) = deny.await else {
        return false;
    };
    audit_denied_tool(url, session_id, "Bash", &reason).await;
    println!("{}", build_pm_guard_deny_response(&reason));
    true
}

/// The whole rule: classify, ask the daemon, decide (#8161).
///
/// Why: the one entry point `pm_guard` calls, so the daemon is asked only
/// after the lexical half has matched and ordinary Bash traffic pays nothing.
/// What: `Some(reason)` to deny, `None` to let the command through.
/// Test: the pure halves below; `pm_guard_denies_a_reset_keep_into_a_live_agents_worktree`
/// in `tests/tm_hook_pm_guard.rs` runs the binary.
pub(crate) async fn linked_worktree_head_move_deny(
    url: &str,
    session_id: &str,
    command: &str,
    cwd: &Path,
    payload: &Value,
    caller_is_subagent: bool,
) -> Option<String> {
    let mv = match classify_linked_worktree_head_move(command, cwd, caller_is_subagent)? {
        LinkedHeadMoveCheck::Deny(reason) => return Some(reason),
        LinkedHeadMoveCheck::Query(mv) => mv,
    };
    let live = tree_holders_or_deny(url, session_id, &[&mv.root, &mv.target], payload).await;
    linked_head_move_verdict(&mv, live.as_deref().map_err(String::as_str))
}

/// Who holds any of `dirs`, counting every session, failing CLOSED.
///
/// Why: `live_shared_tree_writers_in` is fail-open and scoped to other
/// sessions (#6797); a parked worktree needs neither. Both keys are asked for
/// the reason #5769 gave the main-checkout rule — a record may be stamped at the
/// tree root or at the directory the command resolved.
/// What: stops at the first non-empty answer; the first failure is `Err`.
async fn tree_holders_or_deny(
    url: &str,
    session_id: &str,
    dirs: &[&Path],
    payload: &Value,
) -> Result<Vec<String>, String> {
    let mut marked = payload.clone();
    if let Some(object) = marked.as_object_mut() {
        object.insert(TREE_HOLDERS_MARKER.to_string(), Value::Bool(true));
    }
    let mut asked: Vec<&Path> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if asked.contains(dir) {
            continue;
        }
        asked.push(dir);
        let live =
            pm_guard_dispatch::live_shared_tree_writers_or_deny(url, session_id, dir, &marked)
                .await?;
        if !live.is_empty() {
            return Ok(live);
        }
    }
    Ok(Vec::new())
}

/// Deny text for a HEAD move into a worktree a live agent is standing in.
fn live_writer_deny_reason(mv: &LinkedHeadMove, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "HEAD-moving git command denied in a linked worktree (#8161): `git {}` would move HEAD \
         and rewrite the working tree of {}, and the daemon's delegation records name {} as live \
         there. Moving it under a live agent changes the branch its uncommitted work sits on, \
         with no error at any step. Wait for that agent to finish, then consolidate. If the \
         record is stale — the agent ended without its stop reaching the daemon — `tm repair \
         delegation --list {}` shows it and `tm repair delegation <agent-id>` ends it. The \
         recipe is \"Resuming parked work\" in docs/reference/worktree-discipline.md.",
        mv.verb,
        mv.root.display(),
        names.join(", "),
        mv.root.display()
    )
}

/// Deny text for a HEAD move the daemon could not clear (fail closed).
fn unanswered_deny_reason(mv: &LinkedHeadMove, detail: &str) -> String {
    format!(
        "HEAD-moving git command denied in a linked worktree (#8161): `git {}` would move HEAD \
         in {}, and the daemon could not say whether a live agent is writing there — {detail}. \
         Silence is not an idle tree, so this fails closed. Start or restart the daemon (`tm \
         start`, `tm restart`); once it answers, `tm repair delegation --list {}` shows the live \
         records and `tm repair delegation <agent-id>` ends a stale one.",
        mv.verb,
        mv.root.display(),
        mv.root.display()
    )
}

/// Deny text for a HEAD move whose target carries an unresolved expansion.
fn unresolved_deny_reason(verb: &str, shown: &Path, token: &str) -> String {
    format!(
        "HEAD-moving git command denied because its target directory is unresolvable (#8161): \
         `git {verb}` names {}, which still carries the unresolved shell expansion `{token}`, so \
         the guard cannot tell whether it lands in a worktree a live agent is writing in. Spell \
         the directory out: `git -C /abs/path/.claude/worktrees/<name> {verb} …`.",
        shown.display()
    )
}

#[cfg(test)]
#[path = "linked_worktree_head_move_tests.rs"]
mod tests;
