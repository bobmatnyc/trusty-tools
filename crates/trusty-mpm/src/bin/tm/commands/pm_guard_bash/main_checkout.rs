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
//! ([`destructive_target_dir`]) is a main checkout
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
//! Test: `is_whole_tree_destructive_*`, `destructive_target_dir_*` below;
//! `is_main_checkout_*` in `trusty_mpm::core::project_aliases`;
//! `pm_guard_denies_the_incident_commands_in_a_main_checkout` and siblings in
//! `tests/tm_hook_pm_guard.rs` run the stdin→decision→stdout path through the
//! real binary, including the subagent-marked payload.

use std::path::{Path, PathBuf};

use trusty_mpm::core::project_aliases::is_main_checkout;

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
/// when [`destructive_target_dir`] finds a destructive verb whose target
/// directory [`is_main_checkout`]; `None` (ALLOW) otherwise.
/// Test: the two halves are covered separately (see the module doc); the
/// composition runs end to end in `tests/tm_hook_pm_guard.rs`.
pub(crate) fn evaluate_main_checkout_destructive_command(
    command: &str,
    cwd: &Path,
) -> Option<String> {
    let (verb, target) = destructive_target_dir(command, cwd, &PathEnv::from_process())?;
    is_main_checkout(&target).then(|| deny_reason(&verb, &target))
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

/// The directory a whole-tree-destructive git command in `command` would act
/// on, with the verb that made it destructive.
///
/// Why: split from the filesystem classification so the whole text-and-path
/// half — which is where a false positive would come from — is a pure function
/// with no filesystem or environment of its own.
/// What: walks the composition segments (reusing [`split_shell_segments`], so
/// `true && git reset --hard` is classified on its second segment), tracks the
/// effective working directory across `cd` segments and a leading `git -C`
/// exactly as the sibling worktree guard does, and returns the first segment
/// whose git subcommand [`is_whole_tree_destructive`]. `None` when no segment
/// qualifies — including a segment `shlex` cannot split, which yields no argv
/// to classify.
/// Test: `destructive_target_dir_*`.
fn destructive_target_dir(command: &str, cwd: &Path, env: &PathEnv) -> Option<(String, PathBuf)> {
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
        if !is_whole_tree_destructive(&subcommand, &argv[idx + 1..]) {
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

    fn tail(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
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
    fn deny_reason_names_the_verb_the_path_and_the_remedy() {
        let reason = deny_reason("clean", Path::new("/repo/main"));
        assert!(reason.contains("ADR-0037"), "{reason}");
        assert!(reason.contains("git clean"), "{reason}");
        assert!(reason.contains("/repo/main"), "{reason}");
        assert!(reason.contains(".claude/worktrees"), "{reason}");
        assert!(reason.contains(r#"isolation: "worktree""#), "{reason}");
    }
}
