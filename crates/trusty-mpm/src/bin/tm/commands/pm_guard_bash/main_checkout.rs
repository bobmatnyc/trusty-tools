//! Whole-tree-destructive git commands, denied in a project's main checkout
//! ([ADR-0037](../../../../../../../docs/adr/0037-pm-placement-precedence-main-checkout-by-default.md)).
//!
//! Why: ADR-0037's write-restriction amendment makes the main checkout
//! read-only except for documents and configuration, and records that the
//! enforcement "is mechanical and NOT YET BUILT". This module is that
//! mechanism for the destructive-git half. The incident it is built from: a
//! `local-ops` agent dispatched by a DIFFERENT session ran `git checkout <sha>
//! -- .`, `git reset HEAD`, then `git clean -fdx -- crates/ docs/` in the
//! shared main checkout — switching the branch under another live session,
//! discarding uncommitted work, and sweeping build caches across seven UI
//! packages and two crate `target/` directories. Its dispatch brief said that
//! checkout was off-limits. A prohibition in a brief is not enforcement, and a
//! brief is invisible to agents from other sessions.
//!
//! What: [`evaluate_main_checkout_destructive_command`] denies a `Bash` call
//! when BOTH halves hold — the command names a whole-tree-destructive git verb
//! ([`is_whole_tree_destructive`]) AND the directory that verb would act on
//! ([`git_verb_target_dir`]) is a main checkout
//! ([`trusty_mpm::core::project_aliases::is_main_checkout`]). Anything under
//! `.claude/worktrees/**` or `.worktrees/**` is a worktree, never a main
//! checkout, so delegated work stays fully writable — that is where it is
//! supposed to happen.
//!
//! **It pierces the subagent exemptions, deliberately.** Like the
//! `git worktree add` denylist (#3977) and the fan-out denial (#4784), this is
//! called from `pm_guard()` BEFORE Guard 1's and Guard 4's early returns —
//! see that call site's comment. The incident was an AGENT, dispatched by
//! another session, so a PM-only rule would be a no-op for exactly the case it
//! exists for. The owner's 2026-07-26 ruling on #3973 permits this only for a
//! SAFETY rule, and the scope here is drawn to match: irreversible destruction
//! of another session's uncommitted work, nothing wider. It is not a role rule
//! and not a style rule — a subagent keeps every other tool and every
//! non-destructive git verb, in the main checkout and everywhere else.
//!
//! **Fail-open / fail-closed, decided per branch.** The guard consults NO
//! daemon, so it has no unreachable-daemon arm to fail open through — the
//! classification is filesystem-lexical and answers the same way whether or
//! not anything else on the machine is running
//! (`pm_guard_denies_main_checkout_destructive_git_with_the_daemon_unreachable`
//! pins that). The registry was considered and rejected as the authority:
//! `DaemonState::projects` is an in-memory map populated only by an explicit
//! `POST /projects` (or the Telegram `/setproj` command), never from session
//! history, so gating on "registered" would make the guard silently inert on
//! any machine where nobody had registered the path by hand — and the
//! persisted `Project` record carries a `repo_url`, no local path, so it
//! cannot answer the question at all. The remaining indeterminate arms all
//! resolve to ALLOW, and each one is a case where nothing was positively
//! identified: a directory with no `.git` ancestor is not a checkout; a
//! segment `shlex` cannot split (unbalanced quotes) yields no argv to
//! classify; a git verb that is not in the destructive table is not this
//! rule's business. The guard denies only on positive evidence of both halves.
//!
//! Residual bypasses, stated rather than hidden — each is the same shape the
//! sibling `evaluate_worktree_add_command` documents, because both reuse the
//! same lexical path resolution: a `cd`/`-C` argument built from a shell
//! variable or a command substitution is not resolved; a symlink into a
//! checkout is not followed; a destructive verb hidden inside a `$(…)`
//! substitution is not scanned. And the Guard 2/3 operator escape hatches
//! (`TRUSTY_MPM_DISABLE_HOOKS`, `TRUSTY_MPM_PM_UNRESTRICTED`) lift this rule
//! along with every other, which remains tracked as #3981.
//!
//! **The HEAD-moving verbs decide with the daemon, not alone (ADR-0048
//! decision 10).** [`main_checkout_head_move`] classifies `git pull`, `merge`
//! and `rebase` the same lexical way, but it returns the verb and the directory
//! instead of a reason: the caller then asks the daemon's directory-keyed
//! `live_shared_tree_writers` who else is writing there and denies only when
//! the answer is non-empty. That second half is what makes the rule safe to
//! state by verb alone. A solo session updating its own checkout is ordinary
//! work and stays allowed; only a HEAD move under another live writer is
//! refused, and a daemon that cannot answer answers "nobody here", so this
//! branch fails open exactly like the #4480 guard it borrows the query from.
//!
//! **Residuals specific to the HEAD-move rule, stated rather than hidden
//! (#5769).** Four of them, each an ALLOW where a deny might be expected:
//!
//! * `--git-dir=` and `--work-tree=` are skipped when resolving the subcommand
//!   name ([`shell_lex::git_subcommand`]) but are never resolved into a target
//!   directory — only `cd` and `git -C` are. So
//!   `git --git-dir=/repo/.git --work-tree=/repo pull`, run from anywhere,
//!   resolves the running directory rather than `/repo` and is allowed when the
//!   running directory is not itself a shared checkout. Closing it means
//!   teaching [`git_verb_target_dir`] a third override, which the destructive
//!   and commit rules would inherit — a wider change than this rule.
//! * The deny quotes the daemon's delegation records, and a record can be wrong
//!   in the ALLOW direction as well as the deny one. A grant whose isolation
//!   POST failed (that path fails open) leaves a genuinely unisolated record for
//!   an agent that does have a worktree, and the deny then names it. The text
//!   attributes the claim for exactly this reason — see
//!   [`head_move_deny_reason`].
//! * The query is keyed by directory, and this rule asks two — the resolved
//!   target and its checkout root. A record stamped at some THIRD directory
//!   inside the same checkout (a hook that ran from another subdirectory) is
//!   still invisible.
//! * [`IN_PROGRESS_CONTROL_FLAGS`] is matched on the first tail token only, so a
//!   real in-progress control that git would accept in a later position — none
//!   exists today — would be denied rather than exempted. That is the fail
//!   direction this carve-out can afford; the reverse cost a whole `git merge`
//!   its deny.
//!
//! Test: `is_whole_tree_destructive_*`, `destructive_target_dir_*`,
//! `commit_target_dir_*`, `main_checkout_head_move_*`,
//! `starts_a_head_move_*` below;
//! `is_main_checkout_*` in `trusty_mpm::core::project_aliases`;
//! `pm_guard_denies_the_incident_commands_in_a_main_checkout` and siblings in
//! `tests/tm_hook_pm_guard.rs` run the stdin→decision→stdout path through the
//! real binary, including the subagent-marked payload.

use std::path::{Path, PathBuf};

use trusty_mpm::core::project_aliases::{is_main_checkout, main_checkout_root};

use super::{PathEnv, git_dash_c_override, resolve_target_path, split_shell_segments};
use crate::commands::hook_rewrite::first_command_token;
use crate::commands::pm_guard_bash::shell_lex;

/// Deny a whole-tree-destructive git command aimed at a main checkout.
///
/// Why: the one entry point `pm_guard` calls. It is kept to the wrapper shape
/// the sibling worktree guard uses so the process-environment read
/// ([`PathEnv::from_process`]) happens in exactly one place and the policy
/// underneath stays testable without it.
/// What: `Some(reason)` — naming the verb, the directory, and the remedy —
/// when [`git_verb_target_dir`] finds a destructive verb whose target
/// directory [`is_main_checkout`]; `None` (ALLOW) otherwise.
/// Test: the two halves are covered separately (see the module doc); the
/// composition runs end to end in `tests/tm_hook_pm_guard.rs`.
pub(crate) fn evaluate_main_checkout_destructive_command(
    command: &str,
    cwd: &Path,
) -> Option<String> {
    let (verb, target) = git_verb_target_dir(
        command,
        cwd,
        &PathEnv::from_process(),
        is_whole_tree_destructive,
    )?;
    is_main_checkout(&target).then(|| deny_reason(&verb, &target))
}

/// Deny a `git commit` aimed at a main checkout (ADR-0048).
///
/// Why: ADR-0044 makes the main checkout read-only apart from documents and
/// configuration, and a commit is the step that makes a write permanent on a
/// branch other sessions are standing on. The reported incident is exactly
/// this: commit `f1da7bce` landed on `fix/1646-drive-query-v2-migration`, a
/// branch belonging to a different workstream, because three sessions shared
/// one checkout and one of them committed on whichever branch HEAD happened to
/// point at. Blocking the source write alone would not have stopped it — the
/// files were already there.
/// What: `Some(reason)` when a `git commit` segment's effective directory
/// [`is_main_checkout`]; `None` otherwise. Every other git verb, and a commit
/// anywhere else, falls through to ALLOW. `commit` is matched on the verb
/// alone, with no flag conditions: there is no non-writing form of it.
///
/// Scope, stated because the near neighbours are tempting: `git checkout
/// <branch>` and `git switch <branch>` are NOT covered here even though
/// switching a branch under another session is part of the same incident.
/// Their safe and unsafe forms differ by argument rather than by verb and a
/// loose rule there costs a false deny on ordinary work — the failure #5356 was
/// filed for. `pull`, `merge` and `rebase` left that family in ADR-0048
/// decision 10 and are handled by [`main_checkout_head_move`], which needs no
/// argument analysis: none of the three has a form that leaves HEAD alone.
/// Test: `commit_target_dir_*`, `evaluate_main_checkout_commit_*`.
pub(crate) fn evaluate_main_checkout_commit_command(command: &str, cwd: &Path) -> Option<String> {
    let (_, target) = git_verb_target_dir(command, cwd, &PathEnv::from_process(), |verb, _| {
        verb == "commit"
    })?;
    is_main_checkout(&target).then(|| commit_deny_reason(&target))
}

/// The verb and directory of a HEAD-moving git command aimed at a main
/// checkout (ADR-0048 decision 10).
///
/// Why: `pull`, `merge` and `rebase` move HEAD and write the working tree of
/// the directory they run in. In a worktree that HEAD belongs to the one
/// session that owns it, so the move races nothing. In a main checkout the HEAD
/// is shared, and moving it changes the ground another session's uncommitted
/// work is sitting on — with no error at any step, the same silence the commit
/// rule above exists for.
/// What: `Some((verb, directory, checkout_root))` when a segment
/// [`starts_a_head_move`] and the directory it would act on belongs to a main
/// checkout. It returns those rather than a reason because the deny is not
/// decided here: the caller asks the daemon who else is live and denies only on
/// a positive answer. See [`head_move_deny_reason`].
///
/// **Why two directories and not one (#5769).** `directory` is what the command
/// resolves to through `cd` and `git -C`; `checkout_root` is the main checkout
/// that directory sits in. They differ whenever the command runs from a
/// subdirectory, and the delegation records the caller queries are stamped from
/// `tm hook`'s own process directory, which may be either. Both name one HEAD,
/// so the caller asks both — and the test that the directory belongs to a
/// checkout is now [`main_checkout_root`], not [`is_main_checkout`], which
/// closes a second hole in the same place: `cd crates/foo && git pull` resolved
/// a subdirectory, `is_main_checkout` on it was still true, and the query then
/// keyed a directory no record could match.
/// Test: `main_checkout_head_move_*`, and end to end in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) fn main_checkout_head_move(
    command: &str,
    cwd: &Path,
) -> Option<(String, PathBuf, PathBuf)> {
    let (verb, target) =
        git_verb_target_dir(command, cwd, &PathEnv::from_process(), starts_a_head_move)?;
    let root = main_checkout_root(&target)?;
    Some((verb, target, root))
}

/// Flags that operate on an operation already in progress rather than start a
/// new one.
///
/// Why: see [`starts_a_head_move`] — a deny on these would strand a session
/// mid-rebase with no way out, which is the retried-differently-and-worse
/// failure ADR-0048 decision 6 exists to prevent.
const IN_PROGRESS_CONTROL_FLAGS: &[&str] = &[
    "--abort",
    "--quit",
    "--continue",
    "--skip",
    "--edit-todo",
    "--show-current-patch",
];

/// Whether a git subcommand, given its argv tail, would START moving HEAD.
///
/// Why: this is the second false-positive boundary in this module, and the
/// reason ADR-0048 originally left these three verbs uncovered is that a loose
/// rule here costs false denies on ordinary work (#5356). What makes a
/// verb-only rule safe for exactly these three is that none of them has a form
/// that leaves HEAD and the working tree alone: `git pull` is a fetch plus a
/// merge or fast-forward, and `merge`/`rebase` rewrite the current branch.
/// There is no pathspec-vs-ref ambiguity to resolve, which is what keeps
/// `checkout` and `switch` out of this rule and in the doc comment above.
/// Neighbouring verbs are separate subcommand names (`merge-base`,
/// `merge-tree`, `rebase--helper`) and never match.
///
/// The one carve-out is [`IN_PROGRESS_CONTROL_FLAGS`]. Those resolve a state
/// that already exists — the operation started before this call — and denying
/// them would leave the shared checkout parked mid-rebase, which is worse for
/// every other session in it than letting the operation finish or unwind. The
/// deny that matters is the one that stops the move from starting.
///
/// The carve-out is matched POSITIONALLY, on the first tail token only (#5769).
/// Scanning the whole tail exempted any command carrying one of those strings
/// anywhere in it, so `git merge -m "--continue" origin/main` — a real merge,
/// with the flag as a commit message — passed straight through. Git itself
/// accepts these only as the first argument, so the narrower match is also the
/// accurate one.
/// Test: `starts_a_head_move_covers_the_three_verbs`,
/// `starts_a_head_move_allows_in_progress_control`,
/// `starts_a_head_move_matches_in_progress_control_positionally`,
/// `starts_a_head_move_ignores_everything_else`.
fn starts_a_head_move(subcommand: &str, tail: &[String]) -> bool {
    matches!(subcommand, "pull" | "merge" | "rebase")
        && !tail
            .first()
            .is_some_and(|t| IN_PROGRESS_CONTROL_FLAGS.contains(&t.as_str()))
}

/// Build the deny message for a blocked HEAD move.
///
/// Why: as [`deny_reason`] and [`commit_deny_reason`] — ADR-0048 decision 6
/// requires every deny to name what to do instead. This one has two remedies to
/// offer rather than one, and the cheap one comes first: most calls that reach
/// here want updated refs, and `git fetch` gives exactly that with no HEAD move
/// (ADR-0048 decision 9), so a reader who only needed `origin/main` refreshed is
/// unblocked without provisioning anything.
/// What: names the verb, the directory, the sibling agents the daemon reports,
/// `git fetch` as the ref-updating remedy, a worktree as the merge-or-rebase
/// remedy, and the forms this rule never blocks.
///
/// **It attributes, rather than asserts (#5769).** The text used to state as
/// fact that another agent "is already writing there without a worktree of its
/// own". This process cannot know that — it knows only what the daemon's
/// delegation records say, and a record can outlive its agent (nothing closes
/// one whose `SubagentStop` never arrives, for `RUNNING_STALE_AFTER_SECS`) or
/// misdescribe it (a granted dispatch whose isolation POST failed is recorded
/// unisolated, fail-open). Saying whose claim it is keeps the deny honest and
/// tells the reader where to look when it is wrong.
/// Test: `head_move_deny_reason_names_the_verb_the_path_and_both_remedies`,
/// `head_move_deny_reason_attributes_the_claim_to_the_daemon`.
pub(crate) fn head_move_deny_reason(verb: &str, target: &Path, live: &[String]) -> String {
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "HEAD-moving git command denied in a shared main checkout (ADR-0048): `git {verb}` moves \
         HEAD and writes the working tree of {}, and the daemon's delegation records name {} as \
         running there with no worktree of its own — possibly dispatched by a different session \
         standing in the same directory. Moving HEAD under a live writer changes the branch its \
         uncommitted work sits on, and git reports no error when it happens. If you \
         believe that record is stale — the agent finished without its stop signal reaching the \
         daemon — report it to the PM rather than retrying this command. If you need the remote \
         refs updated, run \
         `git fetch` instead: it writes only `refs/remotes/origin/*` and never touches HEAD or \
         the working tree, so it is never blocked here — then branch and diff against \
         `origin/main` rather than local `main`, which only a pull moves. If you need the merge \
         or rebase itself, do it in a worktree: ask the PM to re-dispatch you with \
         `isolation: \"worktree\"`, or `git worktree add .claude/worktrees/<name>`. Read-only git \
         (`status`, `log`, `diff`), `git fetch`, `git rebase --abort`/`--continue`, \
         `git merge --abort`, and everything under `.claude/worktrees/**` are never blocked by \
         this rule.",
        target.display(),
        names.join(", ")
    )
}

/// Build the deny message for a blocked commit.
///
/// Why: as [`deny_reason`] — a bare refusal is retried differently and worse.
/// This one has to be clear that the work is not lost and does not need
/// redoing, only moved, because the reflex on a blocked commit is to reach for
/// `git stash` or a second `-m` attempt.
/// Test: `commit_deny_reason_names_the_path_and_the_remedy`.
fn commit_deny_reason(target: &Path) -> String {
    format!(
        "Commit denied in a main checkout (ADR-0044): {} is a project's main checkout, which is \
         read-only apart from documents and configuration, and other sessions are standing on \
         this same git HEAD. A commit here lands on whichever branch HEAD currently points at — \
         the reported failure is a commit landing on another workstream's branch and the branch \
         it belonged to left empty, with no error at any step. Commit from a worktree instead: \
         ask the PM to re-dispatch you with `isolation: \"worktree\"`, or move the work with \
         `git worktree add .claude/worktrees/<name>` and commit there. Nothing is lost — the \
         changes are still in the tree. Read-only git (`status`, `log`, `diff`) and everything \
         under `.claude/worktrees/**` are never blocked by this rule.",
        target.display()
    )
}

/// Build the deny message.
///
/// Why: a bare refusal leaves the model guessing and it retries the identical
/// call. The text names what was blocked, WHERE (the reader's next question is
/// always "which tree?"), why the directory is special, and the one remedy
/// that always works — do the work in a worktree. Built per call rather than
/// kept as a constant because naming the actual directory is most of its value.
/// Test: `deny_reason_names_the_verb_the_path_and_the_remedy`.
fn deny_reason(verb: &str, target: &Path) -> String {
    format!(
        "Destructive git command denied in a main checkout (ADR-0037): `git {verb}` would \
         overwrite or delete uncommitted work in {}, which is a project's main checkout rather \
         than a worktree. Another session may be standing in that directory, and what this \
         removes is not in git — it cannot be recovered. ADR-0037 makes the main checkout \
         read-only apart from documents and configuration. Do this work in a worktree instead: \
         `git worktree add .claude/worktrees/<name>`, or ask the PM to re-dispatch you with \
         `isolation: \"worktree\"`. Read-only git (`status`, `log`, `diff`), branch creation \
         (`checkout -b`), and a plain `git reset` are never blocked by this rule.",
        target.display()
    )
}

/// The directory the first git segment matching `matches` would act on, with
/// that segment's verb.
///
/// Why: split from the filesystem classification so the whole text-and-path
/// half — which is where a false positive would come from — is a pure function
/// with no filesystem or environment of its own. The classifier is a parameter
/// rather than hardcoded so the destructive-verb rule and the commit rule share
/// one walker: `cd` tracking, `git -C` resolution, and segment splitting are
/// the parts a second copy would drift on, and they are identical for both.
/// What: walks the composition segments (reusing [`split_shell_segments`], so
/// `true && git reset --hard` is classified on its second segment), tracks the
/// effective working directory across `cd` segments and a leading `git -C`,
/// and returns the first segment whose `(verb, argv-tail)` satisfies `matches`.
/// `None` when no segment qualifies — including a segment `shlex` cannot split,
/// which yields no argv to classify.
/// Test: `destructive_target_dir_*`, `commit_target_dir_*`.
fn git_verb_target_dir(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
    matches: impl Fn(&str, &[String]) -> bool,
) -> Option<(String, PathBuf)> {
    let mut effective_cwd = cwd.to_path_buf();
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_command_token(trimmed) == Some("cd") {
            if let Some(argv) = shlex::split(trimmed)
                && let Some(dest) = argv.get(1)
            {
                effective_cwd = resolve_target_path(dest, &effective_cwd, env);
            }
            continue;
        }
        let Some(subcommand) = shell_lex::git_subcommand(trimmed) else {
            continue;
        };
        let Some(argv) = shlex::split(trimmed) else {
            continue;
        };
        // The subcommand token's position, so the tail can be classified.
        // `git_subcommand` already skipped the global flags to resolve the
        // name, and a global flag VALUE that happens to equal the subcommand
        // name (`git -C reset reset --hard`) would find the earlier token —
        // the same "realistic invocations use at most one `-C`" simplification
        // `git_dash_c_override` documents.
        let Some(idx) = argv.iter().position(|t| *t == subcommand) else {
            continue;
        };
        if !matches(&subcommand, &argv[idx + 1..]) {
            continue;
        }
        let base = match git_dash_c_override(&argv, idx) {
            Some(dash_c) => resolve_target_path(dash_c, &effective_cwd, env),
            None => effective_cwd.clone(),
        };
        return Some((subcommand, base));
    }
    None
}

/// Whether a git subcommand, given its argv tail, overwrites or deletes work
/// in the working tree.
///
/// Why: this table IS the false-positive boundary, and #5356 (a `pm_guard`
/// deny landing on a turn of purely read-only `git status`/`git log` calls) is
/// the live reminder of what a loose one costs. So each verb is narrowed to
/// the forms that actually destroy, and everything else — every other
/// subcommand, and every non-destructive form of these five — falls through to
/// ALLOW.
/// What, per verb:
/// - `reset`: only the modes that touch the working tree (`--hard`,
///   `--merge`, `--keep`). A bare `git reset` and its default `--mixed`
///   unstage without destroying anything, and stay allowed.
/// - `clean`: any force flag (`--force`, or a short cluster containing `f`;
///   `x`/`X` too, which are inert without force but cost nothing to include)
///   — UNLESS the command is a dry run (`-n`/`--dry-run`), which only prints.
/// - `checkout`: the pathspec-restoring forms (`-- <pathspec>`, a bare `.`)
///   and `-f`/`--force`, which discards the whole tree. `checkout -b`, a
///   plain branch switch, and a detaching `checkout <sha>` are untouched.
/// - `restore`: the modern equivalent of `checkout -- <pathspec>`, and the
///   easy one to miss. Its DEFAULT target is the working tree, so the rule
///   inverts: destructive unless `--staged`/`-S` is present without
///   `--worktree`/`-W` (index-only, which destroys nothing).
/// - `switch`: `--discard-changes`, plus `-f`/`--force` which implies it.
///   Included so the `checkout`/`switch` pair cannot be used to reach the
///   same destruction through the newer spelling.
///
/// Test: `is_whole_tree_destructive_denies_*`, `is_whole_tree_destructive_allows_*`.
fn is_whole_tree_destructive(subcommand: &str, tail: &[String]) -> bool {
    let has = |names: &[&str]| tail.iter().any(|t| names.contains(&t.as_str()));
    let cluster_has = |chars: &[char]| {
        tail.iter().any(|t| {
            t.starts_with('-')
                && !t.starts_with("--")
                && t.chars().skip(1).any(|c| chars.contains(&c))
        })
    };
    match subcommand {
        "reset" => has(&["--hard", "--merge", "--keep"]),
        "clean" => {
            let dry_run = has(&["--dry-run"]) || cluster_has(&['n']);
            let forced = has(&["--force"]) || cluster_has(&['f', 'x', 'X']);
            forced && !dry_run
        }
        "checkout" => {
            has(&["-f", "--force"])
                || tail.iter().any(|t| t == "." || t == "./")
                || pathspec_separator_with_paths(tail)
        }
        "restore" => {
            let staged = has(&["--staged", "-S"]);
            let worktree = has(&["--worktree", "-W"]);
            worktree || !staged
        }
        "switch" => has(&["--discard-changes", "-f", "--force"]),
        _ => false,
    }
}

/// Whether the tail carries a `--` end-of-options separator with at least one
/// pathspec after it — the `git checkout <ref> -- <pathspec>` shape that
/// restores files over uncommitted work.
fn pathspec_separator_with_paths(tail: &[String]) -> bool {
    tail.iter()
        .position(|t| t == "--")
        .is_some_and(|i| i + 1 < tail.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tail(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    /// The destructive-verb specialisation of [`git_verb_target_dir`], so the
    /// pre-existing cases below keep asserting on the shape they were written
    /// against rather than restating the classifier at every call.
    fn destructive_target_dir(
        command: &str,
        cwd: &Path,
        env: &PathEnv,
    ) -> Option<(String, PathBuf)> {
        git_verb_target_dir(command, cwd, env, is_whole_tree_destructive)
    }

    /// A directory that answers `is_main_checkout`: a `.git` DIRECTORY, which
    /// is how git marks a main checkout and never a linked worktree.
    fn main_checkout_dir() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
        dir
    }

    #[test]
    fn is_whole_tree_destructive_denies_the_incident_shapes() {
        // The two commands from the 2026-08-10 incident that this rule owns.
        // The third, `git reset HEAD`, is deliberately NOT here — see
        // `is_whole_tree_destructive_allows_the_near_misses`.
        assert!(is_whole_tree_destructive(
            "checkout",
            &tail(&["a1b2c3d", "--", "."])
        ));
        assert!(is_whole_tree_destructive(
            "clean",
            &tail(&["-fdx", "--", "crates/", "docs/"])
        ));
    }

    #[test]
    fn is_whole_tree_destructive_denies_every_listed_form() {
        for (verb, args) in [
            ("checkout", vec!["--", "src/lib.rs"]),
            ("checkout", vec!["HEAD~1", "--", "crates/"]),
            ("checkout", vec!["."]),
            ("checkout", vec!["./"]),
            ("checkout", vec!["-f", "main"]),
            ("checkout", vec!["--force", "main"]),
            ("restore", vec!["src/lib.rs"]),
            ("restore", vec!["--source=HEAD~1", "--", "src/"]),
            ("restore", vec!["--staged", "--worktree", "src/"]),
            ("restore", vec!["-S", "-W", "src/"]),
            ("reset", vec!["--hard"]),
            ("reset", vec!["--hard", "origin/main"]),
            ("reset", vec!["--merge", "HEAD"]),
            ("reset", vec!["--keep", "HEAD~2"]),
            ("clean", vec!["-f"]),
            ("clean", vec!["-fd"]),
            ("clean", vec!["-fdx"]),
            ("clean", vec!["-x"]),
            ("clean", vec!["--force", "-d"]),
            ("switch", vec!["--discard-changes", "main"]),
            ("switch", vec!["-f", "main"]),
        ] {
            assert!(
                is_whole_tree_destructive(verb, &tail(&args)),
                "expected `git {verb} {}` to be destructive",
                args.join(" ")
            );
        }
    }

    #[test]
    fn is_whole_tree_destructive_allows_the_near_misses() {
        // The false-positive boundary, and the reason it is tested: #5356 is
        // an open P2 filed because pm_guard denied a turn of purely read-only
        // git calls. `git reset HEAD` is here on the owner's explicit rule —
        // the default `--mixed` unstages and destroys nothing — even though it
        // was one of the three commands the incident agent ran.
        for (verb, args) in [
            ("reset", vec!["HEAD"]),
            ("reset", vec![]),
            ("reset", vec!["--soft", "HEAD~1"]),
            ("reset", vec!["--mixed", "HEAD"]),
            ("checkout", vec!["-b", "feature/x"]),
            ("checkout", vec!["-B", "feature/x", "origin/main"]),
            ("checkout", vec!["main"]),
            ("checkout", vec!["a1b2c3d"]),
            ("restore", vec!["--staged", "src/lib.rs"]),
            ("restore", vec!["-S", "src/lib.rs"]),
            ("clean", vec!["-n"]),
            ("clean", vec!["-nfd"]),
            ("clean", vec!["--dry-run", "-fdx"]),
            ("clean", vec!["-d"]),
            ("switch", vec!["main"]),
            ("switch", vec!["-c", "feature/x"]),
            ("status", vec!["--short"]),
            ("log", vec!["--oneline", "-20"]),
            ("diff", vec!["--stat", "HEAD~1"]),
            ("stash", vec!["list"]),
            ("add", vec!["-A"]),
            ("commit", vec!["-m", "wip"]),
        ] {
            assert!(
                !is_whole_tree_destructive(verb, &tail(&args)),
                "`git {verb} {}` must NOT be treated as destructive",
                args.join(" ")
            );
        }
    }

    #[test]
    fn destructive_target_dir_defaults_to_the_hook_cwd() {
        let env = PathEnv::from_process();
        let (verb, dir) =
            destructive_target_dir("git reset --hard origin/main", Path::new("/repo"), &env)
                .expect("a destructive verb must resolve a target");
        assert_eq!(verb, "reset");
        assert_eq!(dir, PathBuf::from("/repo"));
    }

    #[test]
    fn destructive_target_dir_follows_cd_and_dash_c() {
        // Both bypasses the sibling worktree guard already closes: a `cd` in an
        // earlier segment, and git's own `-C` working-directory override.
        let env = PathEnv::from_process();
        let (_, via_cd) = destructive_target_dir(
            "cd /elsewhere/main && git clean -fdx",
            Path::new("/repo"),
            &env,
        )
        .expect("cd must move the target");
        assert_eq!(via_cd, PathBuf::from("/elsewhere/main"));

        let (_, via_dash_c) = destructive_target_dir(
            "git -C /elsewhere/main reset --hard",
            Path::new("/repo"),
            &env,
        )
        .expect("-C must move the target");
        assert_eq!(via_dash_c, PathBuf::from("/elsewhere/main"));

        // Relative targets resolve against the running directory, and `..`
        // collapses lexically.
        let (_, relative) =
            destructive_target_dir("git -C ../other reset --hard", Path::new("/a/b"), &env)
                .expect("relative -C must resolve");
        assert_eq!(relative, PathBuf::from("/a/other"));
    }

    #[test]
    fn destructive_target_dir_finds_a_verb_hidden_in_a_later_segment() {
        // A benign leading verb must not hide the destructive one behind it.
        let env = PathEnv::from_process();
        let (verb, _) = destructive_target_dir(
            "git status --short; git checkout -- crates/",
            Path::new("/repo"),
            &env,
        )
        .expect("the second segment must be classified");
        assert_eq!(verb, "checkout");
    }

    #[test]
    fn destructive_target_dir_is_none_for_ordinary_work() {
        // The overwhelmingly common traffic: nothing here may reach the
        // filesystem classification at all.
        let env = PathEnv::from_process();
        for command in [
            "git status",
            "git log --oneline -20",
            "git diff HEAD~1",
            "ls -la",
            "cargo test -p trusty-mpm",
            "git checkout -b feature/x",
            "git reset HEAD",
            "git commit -m 'x'",
            "",
            // Unbalanced quotes: shlex yields no argv, so nothing is
            // positively identified and the rule stays out of it.
            "git reset --hard 'unterminated",
        ] {
            assert!(
                destructive_target_dir(command, Path::new("/repo"), &env).is_none(),
                "`{command}` must not resolve a destructive target"
            );
        }
    }

    #[test]
    fn commit_target_dir_finds_the_commit_and_follows_cd_and_dash_c() {
        let env = PathEnv::from_process();
        let is_commit = |verb: &str, _: &[String]| verb == "commit";

        let (verb, dir) =
            git_verb_target_dir("git commit -m 'wip'", Path::new("/repo"), &env, is_commit)
                .expect("a commit must resolve a target");
        assert_eq!(verb, "commit");
        assert_eq!(dir, PathBuf::from("/repo"));

        // The composition shape `git add -A && git commit -m …` is the ordinary
        // one, so the commit must be found in a later segment.
        let (_, chained) = git_verb_target_dir(
            "git add -A && git commit -m 'wip'",
            Path::new("/repo"),
            &env,
            is_commit,
        )
        .expect("the second segment must be classified");
        assert_eq!(chained, PathBuf::from("/repo"));

        // Both directory overrides the destructive rule already closes.
        let (_, via_cd) = git_verb_target_dir(
            "cd /elsewhere/main && git commit -m x",
            Path::new("/repo"),
            &env,
            is_commit,
        )
        .expect("cd must move the target");
        assert_eq!(via_cd, PathBuf::from("/elsewhere/main"));

        let (_, via_dash_c) = git_verb_target_dir(
            "git -C /elsewhere/main commit -m x",
            Path::new("/repo"),
            &env,
            is_commit,
        )
        .expect("-C must move the target");
        assert_eq!(via_dash_c, PathBuf::from("/elsewhere/main"));
    }

    #[test]
    fn commit_target_dir_is_none_for_everything_else() {
        let env = PathEnv::from_process();
        let is_commit = |verb: &str, _: &[String]| verb == "commit";
        for command in [
            "git status",
            "git log --oneline -5",
            "git add -A",
            "git push origin HEAD",
            "cargo test -p trusty-mpm",
            "",
        ] {
            assert!(
                git_verb_target_dir(command, Path::new("/repo"), &env, is_commit).is_none(),
                "`{command}` is not a commit"
            );
        }
    }

    #[test]
    fn evaluate_main_checkout_commit_denies_in_a_checkout_and_allows_in_a_worktree() {
        let checkout = main_checkout_dir();
        let reason = evaluate_main_checkout_commit_command("git commit -m 'wip'", checkout.path())
            .expect("a commit in a main checkout must be denied");
        assert!(reason.contains("ADR-0044"), "{reason}");

        // A linked worktree carries a `.git` FILE. Committing there is the
        // whole remedy the deny offers, so it must work.
        let worktree = tempfile::tempdir().expect("tempdir");
        std::fs::write(worktree.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        assert_eq!(
            evaluate_main_checkout_commit_command("git commit -m 'wip'", worktree.path()),
            None
        );

        // Not a repository at all: nothing to protect.
        let plain = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            evaluate_main_checkout_commit_command("git commit -m 'wip'", plain.path()),
            None
        );
    }

    #[test]
    fn commit_deny_reason_names_the_path_and_the_remedy() {
        let reason = commit_deny_reason(Path::new("/repo/main"));
        assert!(reason.contains("/repo/main"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
        assert!(reason.contains("Nothing is lost"), "{reason}");
    }

    #[test]
    fn starts_a_head_move_covers_the_three_verbs() {
        // ADR-0048 decision 10. Every form of these three moves HEAD, which is
        // what lets the rule be stated by verb with no argument analysis.
        for (verb, args) in [
            ("pull", vec![]),
            ("pull", vec!["--rebase"]),
            ("pull", vec!["origin", "main"]),
            ("pull", vec!["--ff-only"]),
            ("merge", vec!["origin/main"]),
            ("merge", vec!["--no-ff", "feature/x"]),
            ("merge", vec!["--squash", "feature/x"]),
            ("rebase", vec!["origin/main"]),
            ("rebase", vec!["-i", "HEAD~3"]),
            ("rebase", vec!["--onto", "main", "feature/x"]),
        ] {
            assert!(
                starts_a_head_move(verb, &tail(&args)),
                "expected `git {verb} {}` to start a HEAD move",
                args.join(" ")
            );
        }
    }

    #[test]
    fn starts_a_head_move_allows_in_progress_control() {
        // The carve-out: these resolve an operation that already started.
        // Denying them would park the shared checkout mid-rebase with no way
        // out, and the deny would carry no remedy that works from there.
        for (verb, args) in [
            ("rebase", vec!["--abort"]),
            ("rebase", vec!["--continue"]),
            ("rebase", vec!["--skip"]),
            ("rebase", vec!["--quit"]),
            ("rebase", vec!["--edit-todo"]),
            ("rebase", vec!["--show-current-patch"]),
            ("merge", vec!["--abort"]),
            ("merge", vec!["--quit"]),
            ("merge", vec!["--continue"]),
        ] {
            assert!(
                !starts_a_head_move(verb, &tail(&args)),
                "`git {verb} {}` resolves an in-progress operation and must be allowed",
                args.join(" ")
            );
        }
    }

    #[test]
    fn starts_a_head_move_ignores_everything_else() {
        // The false-positive boundary #5356 is the reminder for. `fetch` is the
        // remedy the deny offers and must never be classified (ADR-0048
        // decision 9); `checkout`/`switch` are deliberately out of this rule;
        // `merge-base` and `merge-tree` are separate subcommand names that read
        // without writing.
        for (verb, args) in [
            ("fetch", vec!["origin"]),
            ("fetch", vec!["--all", "--prune"]),
            ("status", vec!["--short"]),
            ("log", vec!["--oneline", "-20"]),
            ("diff", vec!["--stat"]),
            ("merge-base", vec!["main", "HEAD"]),
            ("merge-tree", vec!["main", "HEAD"]),
            ("checkout", vec!["main"]),
            ("switch", vec!["main"]),
            ("pull-request", vec![]),
            ("commit", vec!["-m", "wip"]),
        ] {
            assert!(
                !starts_a_head_move(verb, &tail(&args)),
                "`git {verb} {}` must not be treated as a HEAD move",
                args.join(" ")
            );
        }
    }

    #[test]
    fn starts_a_head_move_matches_in_progress_control_positionally() {
        // #5769: the carve-out used to scan the whole tail, so any command
        // carrying one of those strings anywhere — a commit message is the
        // realistic shape — exempted itself from the rule.
        for args in [
            vec!["-m", "--continue", "origin/main"],
            vec!["origin/main", "-m", "--abort"],
        ] {
            assert!(
                starts_a_head_move("merge", &tail(&args)),
                "`git merge {}` is a real merge and must still classify",
                args.join(" ")
            );
        }
        // The genuine form — the flag first — stays exempt.
        assert!(!starts_a_head_move("rebase", &tail(&["--continue"])));
    }

    #[test]
    fn main_checkout_head_move_resolves_a_subdirectory_to_the_checkout_root() {
        // #5769: a delegation record is stamped from `tm hook`'s own process
        // directory, so a command aimed at a subdirectory has to report the
        // checkout root as well or the query keys a directory no record matches.
        let checkout = main_checkout_dir();
        let sub = checkout.path().join("crates/foo");
        std::fs::create_dir_all(&sub).expect("mkdir sub");
        let (verb, target, root) = main_checkout_head_move(
            &format!("cd {} && git pull", sub.display()),
            checkout.path(),
        )
        .expect("a pull from a subdirectory must resolve");
        assert_eq!(verb, "pull");
        assert_eq!(target, sub);
        assert_eq!(root, checkout.path());
    }

    #[test]
    fn main_checkout_head_move_finds_the_checkout_and_skips_a_worktree() {
        let checkout = main_checkout_dir();
        let (verb, target, root) = main_checkout_head_move("git pull --rebase", checkout.path())
            .expect("a pull in a main checkout must resolve");
        assert_eq!(verb, "pull");
        assert_eq!(target, checkout.path());
        assert_eq!(root, checkout.path());

        // A linked worktree carries a `.git` FILE. Its HEAD belongs to the one
        // session that owns it, so a pull there races nothing and must resolve
        // nothing — this is the ordinary-work case the directory test protects.
        let worktree = tempfile::tempdir().expect("tempdir");
        std::fs::write(worktree.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        assert_eq!(main_checkout_head_move("git pull", worktree.path()), None);

        // Not a repository at all: nothing to protect.
        let plain = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            main_checkout_head_move("git merge main", plain.path()),
            None
        );
    }

    #[test]
    fn main_checkout_head_move_follows_cd_and_dash_c_and_later_segments() {
        let checkout = main_checkout_dir();
        let path = checkout.path().display();
        let outside = tempfile::tempdir().expect("tempdir");

        // `-C` and `cd` both aim the verb at a checkout the hook is not
        // standing in — the two overrides the sibling rules already close.
        let (_, via_dash_c, _) =
            main_checkout_head_move(&format!("git -C {path} rebase origin/main"), outside.path())
                .expect("-C must move the target");
        assert_eq!(via_dash_c, checkout.path());

        let (_, via_cd, _) =
            main_checkout_head_move(&format!("cd {path} && git pull"), outside.path())
                .expect("cd must move the target");
        assert_eq!(via_cd, checkout.path());

        // A benign leading verb must not hide the HEAD move behind it — this is
        // the ordinary `git fetch && git pull` shape.
        let (verb, _, _) = main_checkout_head_move("git fetch origin && git pull", checkout.path())
            .expect("the second segment must be classified");
        assert_eq!(verb, "pull");
    }

    #[test]
    fn main_checkout_head_move_is_none_for_ordinary_work_in_a_checkout() {
        // Even inside a main checkout, everything that is not one of the three
        // verbs resolves nothing and never reaches the daemon query.
        let checkout = main_checkout_dir();
        for command in [
            "git fetch origin",
            "git status --porcelain",
            "git log --oneline -5",
            "git worktree list",
            "cargo test -p trusty-mpm",
            "git rebase --abort",
            "",
        ] {
            assert_eq!(
                main_checkout_head_move(command, checkout.path()),
                None,
                "`{command}` must not resolve a HEAD move"
            );
        }
    }

    #[test]
    fn head_move_deny_reason_names_the_verb_the_path_and_both_remedies() {
        let reason = head_move_deny_reason(
            "pull",
            Path::new("/repo/main"),
            &["rust-engineer".to_string(), "rust-engineer".to_string()],
        );
        assert!(reason.contains("ADR-0048"), "{reason}");
        assert!(reason.contains("git pull"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        // The sibling is named once, not repeated per delegation.
        assert_eq!(reason.matches("rust-engineer").count(), 1, "{reason}");
        // Both remedies: the cheap one (fetch) and the general one (worktree).
        assert!(reason.contains("git fetch"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }

    #[test]
    fn head_move_deny_reason_attributes_the_claim_to_the_daemon() {
        // #5769: this process knows what the daemon's records SAY, never what
        // another agent is doing. A record can outlive its agent, and a grant
        // whose isolation POST failed is recorded unisolated — so stating the
        // claim as fact made the deny assert something it could not check.
        let reason = head_move_deny_reason("pull", Path::new("/repo/main"), &["qa".to_string()]);
        assert!(
            reason.contains("the daemon's delegation records name"),
            "the claim must be attributed to its source: {reason}"
        );
        assert!(
            !reason.contains("is already writing there"),
            "the deny must not assert another agent's behaviour as fact: {reason}"
        );
    }

    #[test]
    fn deny_reason_names_the_verb_the_path_and_the_remedy() {
        let reason = deny_reason("clean", Path::new("/repo/main"));
        assert!(reason.contains("ADR-0037"), "{reason}");
        assert!(reason.contains("git clean"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        assert!(reason.contains(".claude/worktrees"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }
}
