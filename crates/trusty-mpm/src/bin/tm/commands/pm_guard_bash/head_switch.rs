//! HEAD-switching git commands, refused to an AGENT in a dirty main checkout
//! (#8572).
//!
//! Why: a `version-control` dispatch asked to push a branch and open a PR ran
//! `git checkout <branch>` in the shared main checkout while it held the
//! operator's uncommitted edit (reflog: `checkout: moving from main to
//! feat/non-exhaustive-137`). The edit survived only because the file was the
//! same on both branches. The brief forbade the checkout; a brief is not
//! enforcement. The sibling rules in [`super::main_checkout`] left `checkout
//! <branch>` and `switch` alone on purpose, because their safe and unsafe forms
//! differ by argument and state, and a verb-only deny costs the #5356 false
//! deny on ordinary work.
//!
//! What: [`evaluate_main_checkout_head_switch`] denies a `Bash` call when every
//! half holds:
//!
//! 1. the caller is a dispatched subagent — the PM and the operator keep their
//!    own checkout (tm-workflow's orphan-preservation recipe is the PM's);
//! 2. a segment [`switches_head`] — a branch checkout or switch, a new-branch
//!    `checkout -b`/`switch -c`, a detach, a `git stash` that moves work out of
//!    the tree, or a `git bisect` step;
//! 3. that segment's directory belongs to a main checkout, not a harness
//!    worktree and not a disposable scratchpad clone;
//! 4. the checkout holds uncommitted work, OR git cannot say whether it does.
//!
//! Half 4 fails CLOSED: [`has_uncommitted_changes`] answers `None` when
//! `git status --porcelain` cannot run or exits non-zero, and `None` refuses.
//! A clean checkout is allowed, which keeps #5356's boundary. Every segment of a
//! composed command is judged, not only the first. Path restores
//! (`git checkout <sha> -- <path>`) are never this rule's: they are the
//! destructive rule's in a main checkout, and allowed in the agent's own
//! worktree (#8579).
//!
//! Placement: `pm_guard` calls this ahead of Guards 1 and 4, like the
//! destructive rule, because both exemptions return ALLOW for exactly the
//! dispatched agents this rule binds.
//!
//! A `cd`/`-C` directory the guard cannot expand — a `$NAME`, a surviving `~`,
//! a `$(…)` or a backtick — is refused before any other test, from any cwd: an
//! agent in its own worktree writing `git -C $MAIN checkout x` resolves to
//! `<worktree>/$MAIN`, which reads as the worktree while the shell lands it in
//! the main checkout.
//!
//! Residuals, the same lexical ones [`super::main_checkout`] states:
//! `--git-dir=`/`--work-tree=` and the `GIT_DIR=` prefix are not resolved into
//! the target, a symlink into a checkout is not followed, and a verb inside
//! `$(…)` is not scanned.
//!
//! Test: `switches_head_*`, `head_switch_*` below;
//! `pm_guard_refuses_an_agent_branch_switch_in_a_dirty_main_checkout` and
//! siblings in `tests/tm_hook_pm_guard_head_switch.rs` run the real binary.

use std::path::Path;

use trusty_mpm::core::project_aliases::main_checkout_root;
use trusty_mpm::core::uncommitted_changes::has_uncommitted_changes;

use super::main_checkout::git_verb_targets_with_tail;
use super::{PathEnv, unresolved_target};
use crate::commands::pm_guard_write_boundary::write_lands_in_a_scratchpad_clone;

/// Deny an agent's HEAD-switching git command in a dirty main checkout.
///
/// Why: the one entry point `pm_guard` calls; see the module doc.
/// What: `None` (ALLOW) for a non-agent caller without touching git; otherwise
/// [`evaluate_head_switch_in`] with the process environment and the real
/// dirty-tree probe.
/// Test: `head_switch_is_agent_only`, and end to end in
/// `tests/tm_hook_pm_guard_head_switch.rs`.
pub(crate) fn evaluate_main_checkout_head_switch(
    command: &str,
    cwd: &Path,
    caller_is_subagent: bool,
) -> Option<String> {
    if !caller_is_subagent {
        return None;
    }
    evaluate_head_switch_in(
        command,
        cwd,
        &PathEnv::from_process(),
        has_uncommitted_changes,
    )
}

/// The policy, with the environment and the dirty-tree probe injected.
///
/// What: for each segment that [`switches_head`], refuses an unresolved
/// directory outright, skips a directory outside any main checkout or inside a
/// scratchpad clone, and otherwise asks `dirty` about the checkout root:
/// `Some(false)` allows, `Some(true)` and `None` refuse.
/// Test: `head_switch_denies_a_dirty_main_checkout`,
/// `head_switch_allows_a_clean_main_checkout`,
/// `head_switch_fails_closed_when_the_dirty_state_is_unreadable`,
/// `head_switch_allows_the_agents_own_worktree`,
/// `head_switch_judges_every_segment`,
/// `head_switch_refuses_an_unresolved_directory`,
/// `head_switch_refuses_an_unresolved_directory_from_a_worktree`,
/// `head_switch_allows_a_scratchpad_clone`.
fn evaluate_head_switch_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
    dirty: impl Fn(&Path) -> Option<bool>,
) -> Option<String> {
    for (verb, target, _) in git_verb_targets_with_tail(command, cwd, env, switches_head) {
        // #8572: before any classification of `target`. From a worktree cwd,
        // `-C $MAIN` resolves to `<worktree>/$MAIN`, which reads as the
        // agent's own worktree while the shell lands it in the main checkout.
        if let Some((shown, token)) = unresolved_directory(&target) {
            return Some(unresolved_deny_reason(&verb, &shown, &token));
        }
        let Some(root) = main_checkout_root(&target) else {
            continue;
        };
        if write_lands_in_a_scratchpad_clone(&target, &root) {
            continue;
        }
        match dirty(&root) {
            Some(false) => continue,
            Some(true) => return Some(deny_reason(&verb, &root, DIRTY)),
            // #8572: fail closed — an unreadable state is never "clean".
            None => return Some(deny_reason(&verb, &root, UNREADABLE)),
        }
    }
    None
}

/// Whether a git subcommand, given its argv tail, moves HEAD or moves work out
/// of the working tree.
///
/// What, per verb:
/// - `checkout`: any form naming a ref or a new branch — a positional token,
///   `-b`/`-B`/`--orphan`/`--detach`, or `-` for the previous branch. Not the
///   path forms: `-- <paths>`, a bare `.`, `-p`/`--patch`,
///   `--pathspec-from-file`, and a bare `git checkout`, which moves nothing.
/// - `switch`: every form with an argument.
/// - `stash`: everything except `list`, `show`, `create`, `store`, `drop` and
///   `clear`, none of which touches HEAD or the working tree.
/// - `bisect`: every step except `log`, `view`, `visualize`, `terms`, `help`.
///
/// Test: `switches_head_covers_branch_switches_stash_and_bisect`,
/// `switches_head_leaves_path_restores_and_reads_alone`.
fn switches_head(subcommand: &str, tail: &[String]) -> bool {
    let first = tail.first().map(String::as_str);
    match subcommand {
        "checkout" => checkout_switches_head(tail),
        "switch" => !tail.is_empty(),
        "stash" => !matches!(
            first,
            Some("list" | "show" | "create" | "store" | "drop" | "clear")
        ),
        "bisect" => {
            first.is_some_and(|f| !matches!(f, "log" | "view" | "visualize" | "terms" | "help"))
        }
        _ => false,
    }
}

/// The `checkout` half of [`switches_head`].
fn checkout_switches_head(tail: &[String]) -> bool {
    // `git checkout [<tree-ish>] -- <paths>` restores files; a `--` with
    // nothing after it (`git checkout main --`) still switches.
    let args = match tail.iter().position(|t| t == "--") {
        Some(i) if i + 1 < tail.len() => return false,
        Some(i) => &tail[..i],
        None => tail,
    };
    let path_form = args.iter().any(|t| {
        matches!(t.as_str(), "." | "./" | "-p" | "--patch") || t.starts_with("--pathspec-from-file")
    });
    !path_form
        && args.iter().any(|t| {
            matches!(t.as_str(), "-b" | "-B" | "--orphan" | "--detach" | "-") || !t.starts_with('-')
        })
}

/// The path to quote and the expansion the guard could not perform in
/// `target`, if any.
///
/// Why: [`unresolved_target`] finds `$NAME` and `~` but not a command
/// substitution, and `git -C $(…) checkout x` is the same bypass (#8572).
/// What: the [`unresolved_target`] answer, else the first `$(` or backtick in
/// the path text, with the whole path shown.
/// Test: `head_switch_refuses_an_unresolved_directory_from_a_worktree`.
fn unresolved_directory(target: &Path) -> Option<(std::path::PathBuf, String)> {
    if let Some(unresolved) = unresolved_target(target) {
        return Some((unresolved.shown, unresolved.token));
    }
    let text = target.to_string_lossy();
    ["$(", "`"]
        .into_iter()
        .find(|token| text.contains(token))
        .map(|token| (target.to_path_buf(), token.to_string()))
}

/// How the dirty arm describes the checkout.
const DIRTY: &str = "holds uncommitted work (`git status --porcelain` lists changes)";

/// How the fail-closed arm describes the checkout.
const UNREADABLE: &str = "could not be checked: `git status --porcelain` failed there, and \
     a state the guard cannot read is refused rather than assumed clean";

/// Build the deny message.
///
/// Why: a bare refusal is retried differently and worse; this names the tree,
/// why it is protected, and what to run instead — including the push-without-
/// checkout path the reported dispatch needed.
/// Test: `head_switch_denies_a_dirty_main_checkout`,
/// `head_switch_fails_closed_when_the_dirty_state_is_unreadable`.
fn deny_reason(verb: &str, root: &Path, state: &str) -> String {
    format!(
        "HEAD switch denied in a main checkout (#8572): `git {verb}` would move HEAD, overwrite \
         files, or move work out of the working tree of {}, a project's main checkout that \
         {state}. That work \
         belongs to whoever stands in this checkout, usually the operator. A branch switch carries \
         it onto another branch, or overwrites or masks it where the branches differ, and git \
         reports no error. Use your own worktree instead: if a worktree already holds the branch, \
         run the command there (`git worktree list` names it); otherwise ask the PM to re-dispatch \
         you with `isolation: \"worktree\"`. Publishing a branch needs no checkout: \
         `git push origin <branch>` works from here, and `tm pr open` runs from the worktree that \
         holds the branch. Read-only git, `git fetch`, `git pull`, `git push`, and everything \
         under `.claude/worktrees/**` stay allowed, including `git checkout <sha> -- <path>` in \
         your own worktree.",
        root.display()
    )
}

/// Build the deny message for a directory the guard could not resolve.
///
/// Test: `head_switch_refuses_an_unresolved_directory`.
fn unresolved_deny_reason(verb: &str, shown: &Path, token: &str) -> String {
    format!(
        "HEAD switch denied because its target directory is unresolvable (#8572): `git {verb}` \
         names {}, which still carries the unresolved shell expansion `{token}`, so the guard \
         cannot tell whether it lands in your worktree or in a main checkout. Spell the directory \
         out (`git -C /abs/path/.claude/worktrees/<name> {verb} …`) or `cd` into the worktree \
         first.",
        shown.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> PathEnv {
        PathEnv {
            tmpdir: None,
            tmp: None,
            home: Some("/Users/nobody".to_string()),
        }
    }

    /// A main checkout: a directory whose `.git` is a directory.
    fn main_checkout() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        (dir, repo)
    }

    fn argv(tail: &[&str]) -> Vec<String> {
        tail.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn switches_head_covers_branch_switches_stash_and_bisect() {
        for (verb, tail) in [
            ("checkout", vec!["main"]),
            ("checkout", vec!["feat/non-exhaustive-137"]),
            ("checkout", vec!["-b", "feature/x"]),
            ("checkout", vec!["-B", "feature/x", "origin/main"]),
            ("checkout", vec!["--detach"]),
            ("checkout", vec!["a1b2c3d"]),
            ("checkout", vec!["-"]),
            ("checkout", vec!["main", "--"]),
            ("checkout", vec!["-q", "main"]),
            ("switch", vec!["main"]),
            ("switch", vec!["-c", "feature/x"]),
            ("stash", vec![]),
            ("stash", vec!["push", "-m", "wip"]),
            ("stash", vec!["-u"]),
            ("stash", vec!["pop"]),
            ("stash", vec!["branch", "x"]),
            ("bisect", vec!["start"]),
        ] {
            assert!(switches_head(verb, &argv(&tail)), "{verb} {tail:?}");
        }
    }

    #[test]
    fn switches_head_leaves_path_restores_and_reads_alone() {
        for (verb, tail) in [
            ("checkout", vec![]),
            ("checkout", vec!["a1b2c3d", "--", "src/lib.rs"]),
            ("checkout", vec!["--", "src/lib.rs"]),
            ("checkout", vec!["."]),
            ("checkout", vec!["-p", "main"]),
            ("checkout", vec!["--pathspec-from-file=list.txt"]),
            ("switch", vec![]),
            ("stash", vec!["list"]),
            ("stash", vec!["show", "-p"]),
            ("bisect", vec!["log"]),
            ("bisect", vec![]),
            ("status", vec![]),
            ("push", vec!["origin", "feat/x"]),
        ] {
            assert!(!switches_head(verb, &argv(&tail)), "{verb} {tail:?}");
        }
    }

    #[test]
    fn head_switch_denies_a_dirty_main_checkout() {
        let (_dir, repo) = main_checkout();
        for command in [
            "git checkout feat/non-exhaustive-137",
            "git switch main",
            "git checkout -b fix/x",
            "git stash && git checkout feat/x",
            // A path restore without `--` is indistinguishable from a branch
            // name, so it is refused too; the reason must cover it.
            "git checkout src/lib.rs",
        ] {
            let reason = evaluate_head_switch_in(command, &repo, &env(), |_| Some(true))
                .unwrap_or_else(|| panic!("`{command}` must be denied"));
            assert!(reason.contains("#8572"), "{reason}");
            assert!(reason.contains(&repo.display().to_string()), "{reason}");
            assert!(reason.contains("uncommitted work"), "{reason}");
            assert!(reason.contains("isolation: \"worktree\""), "{reason}");
            assert!(reason.contains("move HEAD, overwrite files"), "{reason}");
        }
    }

    #[test]
    fn head_switch_allows_a_clean_main_checkout() {
        // #5356: a clean checkout keeps the pre-#8572 answer.
        let (_dir, repo) = main_checkout();
        for command in ["git checkout main", "git switch -c feature/x", "git stash"] {
            assert!(
                evaluate_head_switch_in(command, &repo, &env(), |_| Some(false)).is_none(),
                "`{command}` in a clean checkout must be allowed"
            );
        }
    }

    #[test]
    fn head_switch_fails_closed_when_the_dirty_state_is_unreadable() {
        let (_dir, repo) = main_checkout();
        let reason = evaluate_head_switch_in("git checkout feat/x", &repo, &env(), |_| None)
            .expect("an unreadable state must refuse the switch");
        assert!(reason.contains("could not be checked"), "{reason}");
    }

    #[test]
    fn head_switch_allows_the_agents_own_worktree() {
        // #8579: path restores and switches inside a harness worktree are the
        // agent's own business, dirty or not.
        let (_dir, repo) = main_checkout();
        let wt = repo.join(".claude/worktrees/agent-1");
        std::fs::create_dir_all(&wt).expect("mkdir wt");
        std::fs::write(wt.join(".git"), "gitdir: ../../.git/worktrees/agent-1").expect(".git");
        for command in [
            "git checkout a1b2c3d -- src/lib.rs",
            "git checkout feat/x",
            "git stash push -u -m tag",
        ] {
            assert!(
                evaluate_head_switch_in(command, &wt, &env(), |_| Some(true)).is_none(),
                "`{command}` in the agent's own worktree must be allowed"
            );
        }
        let cmd = format!("git -C {} checkout feat/x", wt.display());
        assert!(evaluate_head_switch_in(&cmd, &repo, &env(), |_| Some(true)).is_none());
    }

    #[test]
    fn head_switch_judges_every_segment() {
        let (_dir, repo) = main_checkout();
        let command = "git -C .claude/worktrees/a checkout x && git checkout feat/y";
        assert!(evaluate_head_switch_in(command, &repo, &env(), |_| Some(true)).is_some());
    }

    #[test]
    fn head_switch_refuses_an_unresolved_directory() {
        let (_dir, repo) = main_checkout();
        let reason =
            evaluate_head_switch_in("git -C $WT checkout feat/x", &repo, &env(), |_| Some(false))
                .expect("an unresolved directory must refuse");
        assert!(reason.contains("$WT"), "{reason}");
    }

    /// 🔴 REGRESSION (#8572 review): from the agent's own worktree, `-C $MAIN`
    /// resolved to `<worktree>/$MAIN`, which read as the worktree, so the
    /// switch was allowed while the shell ran it in the main checkout.
    #[test]
    fn head_switch_refuses_an_unresolved_directory_from_a_worktree() {
        let (_dir, repo) = main_checkout();
        let wt = repo.join(".claude/worktrees/agent-1");
        std::fs::create_dir_all(&wt).expect("mkdir wt");
        std::fs::write(wt.join(".git"), "gitdir: ../../.git/worktrees/agent-1").expect(".git");
        for (command, token) in [
            ("git -C $MAIN checkout feat/x", "$MAIN"),
            ("cd $MAIN && git switch x", "$MAIN"),
            ("git -C \"${MAIN}\" stash", "${MAIN}"),
            ("git -C \"$(cat /tmp/main)\" checkout x", "$("),
            ("cd \"`cat /tmp/main`\" && git checkout x", "`"),
        ] {
            let reason = evaluate_head_switch_in(command, &wt, &env(), |_| Some(false))
                .unwrap_or_else(|| panic!("`{command}` from a worktree must be denied"));
            assert!(reason.contains("unresolvable"), "{reason}");
            assert!(reason.contains(token), "{command}: {reason}");
        }
    }

    #[test]
    fn head_switch_allows_a_scratchpad_clone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let clone = dir.path().join("scratchpad").join("bisect");
        std::fs::create_dir_all(clone.join(".git")).expect("mkdir clone .git");
        assert!(
            evaluate_head_switch_in("git bisect start", &clone, &env(), |_| Some(true)).is_none()
        );
    }

    #[test]
    fn head_switch_is_agent_only() {
        let (_dir, repo) = main_checkout();
        assert!(evaluate_main_checkout_head_switch("git checkout feat/x", &repo, false).is_none());
    }
}
