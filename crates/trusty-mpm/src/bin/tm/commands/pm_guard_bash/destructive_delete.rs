//! `tm hook --pm-guard` — destructive-root deletion guard (#4031).
//!
//! Why: no `rm`/`rmdir`/`unlink`/`find … -delete` verb existed anywhere in
//! [`super::classify_bash_segment`], so those deletions were entirely
//! unenforced — a PM session, or an agent it dispatches, with script drift can
//! recursively delete another session's worktree or in-flight work via
//! `rm -rf`, even though the guard already blocks `Edit`/`Write` on source.
//! This closes the SAFETY subset only (owner ruling on #3973): recursive
//! force-deletion of a filesystem root (`/`, `/root`, `/Users/<name>` on
//! macOS, `/home/<name>` on Linux, `$HOME`), a repository root, a `.git`
//! directory, or a
//! `.claude/worktrees`/`.worktrees` entry. It deliberately does NOT block
//! ordinary local cleanup (`rm stale.txt`), build-artifact cleanup
//! (`cargo clean`), or local temp cleanup (`git clean -fd`) — none of those
//! target a denylisted path.
//! What: [`evaluate_destructive_delete_command`] is a TARGET-PATH classifier
//! — matching only the literal `rm` token would miss `rmdir`/`unlink`/`find
//! -delete`, so instead every composition segment is scanned TOKEN BY TOKEN
//! for one of those four verbs (issue #4031 review, pass 2, item 1). This
//! deliberately does NOT enumerate wrapper words (`sudo`, `env`, `nice`,
//! `time`, `nohup`, `exec`, `command`, `builtin`, `doas`, `ionice`, `timeout`,
//! `stdbuf`, `caffeinate`, …) the way the sibling `cd`-tracker and the git
//! guards do — round 1 of this review enumerated only `sudo`/`env`, then
//! `command`/`builtin`, and each addition missed the next wrapper someone
//! actually used. Scanning every token for the verb itself makes the
//! enumeration moot: no wrapper, known or future, changes which token IS
//! `rm`, only what precedes it. **Over-matching is intentional and
//! ACCEPTED**: this guard denies only when a resolved TARGET lands on the
//! absolute denylist below, so a benign command that merely CONTAINS the word
//! `rm` as a non-verb argument (`echo rm -rf /`) being denied is the safe
//! direction for a safety rule, not a bug to special-case away — the
//! alternative (verb-position enumeration) is exactly the defect this
//! rewrite closes.
//! Once a verb token is found, its DELETION-TARGET argument(s) are resolved
//! against a `cd`-tracked effective working directory (the same [`PathEnv`]
//! expansion and lexical normalization [`super::evaluate_worktree_add_command`]
//! uses) and checked against [`is_denylisted_delete_target`], which also
//! resolves a glob-suffixed target's PARENT ([`glob_parent`]) since this
//! module classifies text and never expands a glob the way the shell would.
//! **A delete verb whose target could not be resolved at all — an
//! unparseable segment, or a verb found with no positional argument — FAILS
//! CLOSED** ([`DESTRUCTIVE_DELETE_UNRESOLVED_REASON`], issue #4031 review item
//! 2): the alternative (silently allow) is exactly how `first_command_token`
//! returning `None` for `env -i rm -rf /root` bypassed the previous
//! iteration of this guard, which depended on it for verb detection.
//! It is called from `pm_guard()` BEFORE Guard 1's and Guard 4's early
//! returns, and applies to every caller — the PM and any subagent alike —
//! matching the worktree-add-tmp and main-checkout-destructive guards'
//! placement, not the PM-exempt shape of the sibling
//! [`super::evaluate_worktree_remove_command`]: a filesystem-root, repo-root,
//! `.git`, or worktree deletion is never legitimate for either caller, so
//! there is no exemption to preserve. `git worktree remove` and `git branch
//! -D` are untouched by this rule — they are different verbs, governed by
//! their own existing rules (`git worktree remove` by #5791, now also
//! wrapper-resistant via `shell_lex::git_subcommand`'s shared
//! `strip_wrapper_prefix`; `git branch -D` by no rule at all, allowed for
//! both PM and subagent, unchanged).
//!
//! Residual bypasses accepted, stated rather than hidden:
//! - Indirection through a shell variable (`X=/; rm -rf "$X"`) or a command
//!   substitution (`rm -rf "$(mktemp -d)"`) is not resolved.
//! - A verb reached via `xargs` or a genuine shell function/alias override of
//!   `rm` itself (not the `\`/`command`/`builtin` bypass idioms, which this
//!   module resolves) is not detected — this scans the command TEXT, it does
//!   not execute the shell or consult its alias table.
//! - A symlink whose target is a denylisted root is not followed — resolution
//!   here is purely lexical, never `fs::canonicalize`, for the same
//!   fast/side-effect-free reason [`super::resolve_target_path`] documents.
//! - The Guard 2/3 operator escape hatches
//!   (`TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED`) lift this rule
//!   along with every other in the file — tracked separately as issue #3981.
//!
//! Test: `denies_filesystem_root_deletion`, `denies_repo_root_deletion`,
//! `denies_dot_git_deletion`, `denies_worktree_root_deletion`,
//! `allows_worktree_interior_paths`, `allows_ordinary_cleanup`,
//! `denies_rmdir_of_a_denylisted_root`, `denies_find_delete_of_a_denylisted_root`,
//! `denies_a_delete_hidden_in_a_composed_command`,
//! `denies_home_expanded_from_a_literal_dollar_home`,
//! `denies_wrapper_words_regardless_of_enumeration`,
//! `denies_bare_container_roots`, `denies_unresolvable_delete_targets`,
//! `allows_over_matched_non_verb_mentions_that_resolve_to_no_target` below;
//! `pm_guard_denies_destructive_delete_of_repo_root` and siblings in
//! `tests/tm_hook_pm_guard.rs` exercise the end-to-end binary path.

use std::path::{Component, Path};

use trusty_mpm::core::project_aliases::main_checkout_root;

use super::{PathEnv, resolve_target_path, split_shell_segments};
use crate::commands::hook_rewrite::first_command_token;

/// Deny reason for `rm`/`rmdir`/`unlink`/`find -delete` targeting a
/// denylisted destructive root (issue #4031).
///
/// Why: a bare refusal invites a retry with a different verb or a hand-rolled
/// workaround, so the text names every denylisted category and the one
/// sanctioned path for the worktree case (the PM's `tm session
/// prune-worktrees`), the same way `WORKTREE_REMOVE_DENY_REASON`
/// does for the sibling `git worktree remove` rule.
/// What: the `permissionDecisionReason` string emitted on this deny.
pub(crate) const DESTRUCTIVE_DELETE_REASON: &str = "`rm`/`rmdir`/`unlink`/`find -delete` must \
     not target a filesystem root or bare container (`/`, `/root`, `/Users`, `/Users/<name>`, \
     `/home`, `/home/<name>`, `/Volumes`, `/private`, `/var`, `/etc`, `/usr`, `/opt`, `/Library`, \
     `/System`, `/Applications`, `$HOME`, or `$HOME`'s parent directory), a repository root, a `.git` \
     directory, or a `.claude/worktrees`/`.worktrees` entry (issue #4031) — each is either \
     unrecoverable data loss or another session's or workstream's uncommitted work. Ordinary file \
     and directory cleanup elsewhere (build artifacts, stale files, `git clean -fd`) is unaffected. \
     To remove a worktree, ask the PM to run `tm session prune-worktrees --merged-prs --force` — \
     `rm -rf` on a worktree directory is never the workaround.";

/// Deny reason when a segment contains a delete verb this classifier cannot
/// resolve a target for — an unparseable segment (unbalanced quotes) that
/// plausibly names one, or a bare verb invocation with no positional
/// argument at all (issue #4031 review, item 2).
///
/// Why: the alternative is silently allowing exactly the shape that bypassed
/// the previous iteration of this guard (`env -i rm -rf /root`, where verb
/// detection depended on [`first_command_token`] and that function
/// conservatively returns `None` rather than guess past an ambiguous flag).
/// A guard whose failure mode is "can't tell, so allow" is not a guard; this
/// one's failure mode is "can't tell, so deny and say so".
/// What: the `permissionDecisionReason` string emitted on this deny.
pub(crate) const DESTRUCTIVE_DELETE_UNRESOLVED_REASON: &str = "A Bash segment names \
     `rm`/`rmdir`/`unlink`/`find` but this guard could not resolve what it targets (issue #4031) \
     — either the segment's quoting could not be parsed, or the verb carried no positional \
     argument. Denying rather than guessing is this guard's fail-closed rule. Rewrite the command \
     with an unambiguous, directly-quoted target.";

/// The four verbs this guard scans every segment's TOKENS for — not just the
/// first/wrapper-resolved one (issue #4031 review, item 1). Everything else
/// falls through to [`super::classify_bash_segment`]'s ordinary rules
/// unchanged.
const DELETE_VERBS: &[&str] = &["rm", "rmdir", "unlink", "find"];

/// Classify a Bash command for destructive-root deletion: `Some(reason)`
/// denies, `None` allows.
///
/// Why: the one entry point `pm_guard` calls, kept to the same
/// process-environment-reading wrapper shape as
/// [`super::evaluate_worktree_add_command`] so the policy underneath stays
/// testable without touching `std::env`.
/// What: delegates to [`evaluate_destructive_delete_command_in`] against the
/// guard process's real environment.
/// Test: see the module doc's test list.
pub(crate) fn evaluate_destructive_delete_command(
    command: &str,
    cwd: &Path,
) -> Option<&'static str> {
    evaluate_destructive_delete_command_in(command, cwd, &PathEnv::from_process())
}

/// [`evaluate_destructive_delete_command`] against an explicit environment —
/// see [`PathEnv`] for why production and tests must not share `std::env`
/// mutation.
fn evaluate_destructive_delete_command_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
) -> Option<&'static str> {
    let mut effective_cwd = cwd.to_path_buf();
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Same `cd`-tracking shape as `evaluate_worktree_add_command_in`: a
        // deliberate, partial closing of `cd /tmp && rm -rf x` — see that
        // function's doc for the residual (shell-variable / substitution
        // built `cd` target) this shares. Unlike verb detection below, this
        // still goes through `first_command_token` — a wrapped `cd` (`nice cd
        // /tmp`) is a real but narrower gap than wrapper-enumerated VERB
        // detection was, since a missed `cd` only leaves `effective_cwd`
        // stale rather than letting a delete verb through unclassified.
        if first_command_token(trimmed) == Some("cd") {
            if let Some(argv) = shlex::split(trimmed)
                && let Some(dest) = argv.get(1)
            {
                effective_cwd = resolve_target_path(dest, &effective_cwd, env);
            }
            continue;
        }
        let Some(argv) = shlex::split(trimmed) else {
            // Unbalanced quotes — cannot tokenize this segment at all. Fail
            // CLOSED (item 2) only when the raw text plausibly names one of
            // the delete verbs as a whole word; an unparseable segment with
            // nothing suspicious in it is simply not this rule's business.
            if segment_mentions_a_delete_verb(trimmed) {
                return Some(DESTRUCTIVE_DELETE_UNRESOLVED_REASON);
            }
            continue;
        };
        // #4031 review, item 1: scan EVERY token for a delete verb — no
        // wrapper enumeration, see the module doc for why. A leading `\` is
        // stripped before comparison (the same alias-bypass idiom
        // `hook_rewrite::strip_wrapper_prefix` resolves).
        let Some(verb_idx) = argv
            .iter()
            .position(|tok| DELETE_VERBS.contains(&tok.strip_prefix('\\').unwrap_or(tok)))
        else {
            continue;
        };
        let verb = argv[verb_idx].strip_prefix('\\').unwrap_or(&argv[verb_idx]);
        let tail = &argv[verb_idx + 1..];
        let targets = delete_targets(verb, tail);
        if verb == "find" && targets.is_empty() {
            // No `-delete` action present — a plain search, not this rule's
            // business (see `delete_targets`).
            continue;
        }
        if targets.is_empty() {
            // rm/rmdir/unlink found but no resolvable positional argument —
            // fail CLOSED (item 2) rather than silently allow.
            return Some(DESTRUCTIVE_DELETE_UNRESOLVED_REASON);
        }
        let repo_root = main_checkout_root(&effective_cwd);
        for target in targets {
            let resolved = resolve_target_path(&target, &effective_cwd, env);
            if is_denylisted_delete_target(&resolved, repo_root.as_deref(), env) {
                return Some(DESTRUCTIVE_DELETE_REASON);
            }
        }
    }
    None
}

/// Whether `text` — a segment [`shlex::split`] could not tokenize — plausibly
/// names one of [`DELETE_VERBS`] as a whole word.
///
/// Why: an unparseable segment (unbalanced quotes) gives no argv to scan, but
/// item 2's fail-closed rule still applies when the raw text looks like it
/// might be a delete invocation — this is the conservative, over-matching
/// fallback for that rare case, not the normal path (which shlex parses).
/// What: splits on any non-alphanumeric/underscore byte (quotes, slashes,
/// dashes, backslashes all separate) and checks the resulting words for an
/// exact match — cruder than [`shlex::split`], deliberately, since a proper
/// parse already failed.
fn segment_mentions_a_delete_verb(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| DELETE_VERBS.contains(&word))
}

/// The path argument(s) a resolved deletion verb's argv tail would act on.
///
/// Why: `rm`/`rmdir`/`unlink` take one or more trailing paths with no
/// value-taking flags, but `find`'s search root(s) are its LEADING positional
/// tokens — anything after the first flag is an expression operand (e.g. the
/// pattern in `-name '*.rs'`), not a path.
/// What: `rm`/`rmdir`/`unlink` → every non-flag token (honoring a `--`
/// end-of-options marker); `find` → the leading non-flag tokens, defaulting to
/// `.` (find's own default search root) when none precede the first flag, and
/// only when `-delete` appears somewhere in `tail` — a `find` with no
/// `-delete` action never deletes anything and is not this rule's business.
fn delete_targets(program: &str, tail: &[String]) -> Vec<String> {
    if program == "find" {
        if !tail.iter().any(|t| t == "-delete") {
            return Vec::new();
        }
        let paths: Vec<String> = tail
            .iter()
            .take_while(|t| !t.starts_with('-'))
            .cloned()
            .collect();
        return if paths.is_empty() {
            vec![".".to_string()]
        } else {
            paths
        };
    }
    let mut out = Vec::new();
    let mut positional_only = false;
    for tok in tail {
        if !positional_only && tok == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && tok.starts_with('-') && tok.len() > 1 {
            continue;
        }
        out.push(tok.clone());
    }
    out
}

/// Whether `path` is one of the destructive-root categories issue #4031
/// denylists.
///
/// What: `true` when `path` — after [`resolve_target_path`]'s expansion and
/// lexical normalization, and after [`glob_parent`]'s glob-aware
/// substitution — is exactly `/`, `/root`, `$HOME`'s resolved value, a
/// single-level `/Users/<name>`/`/home/<name>` home root, the checkout's own
/// repository root
/// (`repo_root`, resolved once per segment by the caller via
/// [`main_checkout_root`] — `None` for a worktree, whose own root is instead
/// caught by [`is_worktree_root_or_container`]), a path whose basename is
/// exactly `.git`, or a `.claude/worktrees`/`.worktrees` entry at or one level
/// below the container directory. Deeper paths — a file or subdirectory
/// inside a worktree, inside `$HOME`, or inside the repo — are NOT denylisted;
/// this is a target-PATH classifier, not a target-root-prefix one, so ordinary
/// cleanup under any of these stays allowed.
fn is_denylisted_delete_target(path: &Path, repo_root: Option<&Path>, env: &PathEnv) -> bool {
    let path = glob_parent(path);
    if path == Path::new("/") || path == Path::new("/root") {
        return true;
    }
    if BARE_CONTAINER_ROOTS
        .iter()
        .any(|root| path == Path::new(root))
    {
        return true;
    }
    if let Some(home) = env.home.as_deref() {
        if path == Path::new(home) {
            return true;
        }
        if let Some(parent) = Path::new(home).parent()
            && path == parent
        {
            return true;
        }
    }
    if is_user_home_root(path) {
        return true;
    }
    if path.file_name().and_then(|f| f.to_str()) == Some(".git") {
        return true;
    }
    if repo_root.is_some_and(|root| path == root) {
        return true;
    }
    is_worktree_root_or_container(path)
}

/// Bare container directories — deleting the whole directory (not a specific
/// entry inside it) destroys every user's / every app's / every mount's data
/// at once (issue #4031 review, item 3). `/Users/<name>` and `/home/<name>`
/// (a SPECIFIC user's home, macOS and Linux respectively) is
/// [`is_user_home_root`]'s separate, narrower check; this list is the
/// container ABOVE that — both platform spellings (`/Users` AND `/home`) are
/// listed here regardless of which OS this guard happens to be running on, so
/// the macOS/Linux parity this list implies is real rather than aspirational
/// (pass 3 of this review: `is_user_home_root` recognized only `/Users/<name>`
/// at first, leaving `/home/<name>` unenforced even though this list already
/// named the bare `/home` container).
///
/// Why: round 1 of this review only denylisted a specific user's home root
/// and the literal filesystem root — `rm -rf /Users` (every user's home at
/// once) and `rm -rf /etc` (system configuration) matched neither and were
/// allowed.
const BARE_CONTAINER_ROOTS: &[&str] = &[
    "/Users",
    "/home",
    "/Volumes",
    "/private",
    "/var",
    "/etc",
    "/usr",
    "/opt",
    "/Library",
    "/System",
    "/Applications",
];

/// The directory a glob-suffixed delete target would actually clear, or
/// `path` itself when it names no glob.
///
/// Why (#4031 review, CRITICAL 2): this module classifies TEXT — it never
/// expands a glob the way the shell would at execution time — so
/// `rm -rf /Users/bob/*` reached [`is_denylisted_delete_target`] as the
/// literal path `/Users/bob/*`, which matched no denylist entry exactly,
/// while the shell's own expansion would delete everything inside
/// `/Users/bob`: exactly as destructive as `rm -rf /Users/bob` itself, and
/// the same shape as `rm -rf ./*`/`rm -rf ~/*` run from `$HOME`, or
/// `rm -rf .[!.]*` (a hidden-file glob). Rather than implement glob
/// expansion (unbounded, and it would have to touch the filesystem), the
/// glob's PARENT directory — where the shell would actually perform the
/// deletion — is evaluated against the denylist instead.
/// What: gated on the LAST path component containing `*`, `?`, or `[`
/// (POSIX glob metacharacters); when it does, returns `path`'s parent —
/// `Path::parent` yields an empty relative path for a bare single component
/// (e.g. a literal `*` with no directory prefix, which in practice never
/// reaches this function unresolved: [`resolve_target_path`] always joins a
/// relative token onto the tracked effective cwd first) — or `path` itself in
/// the (unreachable in practice) case `parent()` returns `None` at all. A
/// non-glob path is returned unchanged.
fn glob_parent(path: &Path) -> &Path {
    let has_glob = path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.contains(['*', '?', '[']));
    if has_glob {
        path.parent().unwrap_or(path)
    } else {
        path
    }
}

/// Whether `path` is exactly a single-level `/Users/<name>` (macOS) or
/// `/home/<name>` (Linux) entry — someone's entire home directory.
///
/// Why (#4031 review, pass 3): the first cut recognized only `/Users/<name>`,
/// so `rm -rf /home/someoneelse` and its glob-suffixed form denied on macOS
/// but allowed on Linux — the same hazard, unenforced on half the platforms
/// this guard runs on. [`BARE_CONTAINER_ROOTS`] already lists both `/Users`
/// and `/home` as the CONTAINER; this is the narrower, one-level-deeper check
/// for a SPECIFIC user's home under either.
/// What: `true` only for `RootDir, Normal("Users" | "home"), Normal(_)` with
/// nothing after — `/Users/bob/Projects/foo` and `/home/bob/project` (an
/// ordinary subdirectory) are NOT matched, only `/Users/bob`/`/home/bob`
/// themselves.
fn is_user_home_root(path: &Path) -> bool {
    let mut comps = path.components();
    matches!(comps.next(), Some(Component::RootDir))
        && matches!(comps.next(), Some(Component::Normal(n)) if matches!(n.to_str(), Some("Users" | "home")))
        && matches!(comps.next(), Some(Component::Normal(_)))
        && comps.next().is_none()
}

/// Whether `path` is a `.claude/worktrees`/`.worktrees` container directory
/// itself, or one specific worktree's root inside it.
///
/// Why: deleting the container destroys every worktree at once; deleting a
/// direct child destroys one. A path deeper than that — a file or directory
/// INSIDE a worktree — is ordinary work happening exactly where it is
/// supposed to (issue #3977) and must stay allowed, so this does not match a
/// bare path-contains check the way `project_aliases::is_worktree_path`
/// does for its coarser "is this a worktree at all" question.
/// What: finds the first `.worktrees` component, or the first adjacent
/// `.claude`, `worktrees` pair, and denies only when at most one path
/// component remains after it.
fn is_worktree_root_or_container(path: &Path) -> bool {
    let comps: Vec<Component<'_>> = path.components().collect();
    for (i, component) in comps.iter().enumerate() {
        let Component::Normal(name) = component else {
            continue;
        };
        let container_end = if name.to_str() == Some(".worktrees") {
            i + 1
        } else if name.to_str() == Some(".claude")
            && matches!(comps.get(i + 1), Some(Component::Normal(n)) if n.to_str() == Some("worktrees"))
        {
            i + 2
        } else {
            continue;
        };
        return comps.len() - container_end <= 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_home(home: &str) -> PathEnv {
        // #4031: PathEnv's fields are module-private, not per-field
        // constructors — this test lives in the same module tree
        // (`pm_guard_bash::destructive_delete`) as `PathEnv` itself, so
        // constructing one directly here is the same seam
        // `evaluate_worktree_add_command_expands_tmpdir_and_home` in
        // `super::tests` uses.
        PathEnv {
            tmpdir: None,
            tmp: None,
            home: Some(home.to_string()),
        }
    }

    #[test]
    fn denies_filesystem_root_deletion() {
        let env = env_with_home("/Users/agent");
        for command in [
            "rm -rf /",
            "rm -rf /root",
            "rm -rf $HOME",
            "rm -rf /Users/someone",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/work"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn denies_home_expanded_from_a_literal_dollar_home() {
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in("rm -rf ${HOME}", Path::new("/work"), &env),
            Some(DESTRUCTIVE_DELETE_REASON)
        );
    }

    #[test]
    fn denies_repo_root_deletion() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(repo.path().join(".git")).expect(".git dir");
        let env = env_with_home("/Users/agent");
        let command = format!("rm -rf {}", repo.path().display());
        assert_eq!(
            evaluate_destructive_delete_command_in(&command, repo.path(), &env),
            Some(DESTRUCTIVE_DELETE_REASON)
        );
    }

    #[test]
    fn denies_dot_git_deletion() {
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in("rm -rf .git", Path::new("/repo"), &env),
            Some(DESTRUCTIVE_DELETE_REASON)
        );
    }

    #[test]
    fn denies_worktree_root_deletion() {
        let env = env_with_home("/Users/agent");
        for command in [
            "rm -rf /repo/.claude/worktrees/agent-x",
            "rm -rf /repo/.claude/worktrees",
            "rm -rf /repo/.worktrees/agent-x",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn allows_worktree_interior_paths() {
        let env = env_with_home("/Users/agent");
        // A file or directory INSIDE a worktree is ordinary cleanup, not the
        // hazard this rule closes.
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "rm -rf /repo/.claude/worktrees/agent-x/target",
                Path::new("/repo"),
                &env
            ),
            None
        );
    }

    #[test]
    fn allows_ordinary_cleanup() {
        let env = env_with_home("/Users/agent");
        for command in [
            "rm stale.txt",
            "rm -rf crates/x/target",
            "cargo clean",
            "git clean -fd",
            "rmdir empty-dir",
            "",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                None,
                "expected allow for: {command}"
            );
        }
    }

    #[test]
    fn denies_rmdir_of_a_denylisted_root() {
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "rmdir /repo/.claude/worktrees/agent-x",
                Path::new("/repo"),
                &env
            ),
            Some(DESTRUCTIVE_DELETE_REASON)
        );
    }

    #[test]
    fn denies_unlink_of_a_denylisted_root() {
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in("unlink /root", Path::new("/repo"), &env),
            Some(DESTRUCTIVE_DELETE_REASON)
        );
    }

    #[test]
    fn denies_find_delete_of_a_denylisted_root() {
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "find /repo/.git -delete",
                Path::new("/repo"),
                &env
            ),
            Some(DESTRUCTIVE_DELETE_REASON)
        );
    }

    #[test]
    fn allows_find_without_delete_action() {
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "find /repo/.git -name '*.pack'",
                Path::new("/repo"),
                &env
            ),
            None
        );
    }

    #[test]
    fn denies_a_delete_hidden_in_a_composed_command() {
        let env = env_with_home("/Users/agent");
        for command in [
            "cargo test -p trusty-mpm && rm -rf /root",
            "true; rm -rf .git",
            "cd /repo && rm -rf .claude/worktrees/agent-x",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn denies_pwd_expansion_of_the_current_worktree_root() {
        // #4031 review, CRITICAL 1: `$PWD` reached `resolve_target_path`
        // unexpanded, so `rm -rf $PWD` from inside a session's own worktree
        // root matched no denylist entry. `$PWD` must expand to the tracked
        // effective cwd, not merely allow (the same property the sibling
        // `$HOME` expansion test pins).
        let env = env_with_home("/Users/agent");
        let worktree = Path::new("/repo/.claude/worktrees/agent-x");
        for command in ["rm -rf $PWD", "rm -rf ${PWD}"] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, worktree, &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn allows_pwd_expansion_of_a_subdirectory() {
        // The other half: `$PWD/target` is an ordinary subdirectory of the
        // worktree, not its root — must stay allowed.
        let env = env_with_home("/Users/agent");
        let worktree = Path::new("/repo/.claude/worktrees/agent-x");
        assert_eq!(
            evaluate_destructive_delete_command_in("rm -rf $PWD/target", worktree, &env),
            None
        );
    }

    #[test]
    fn denies_glob_suffixed_deletes_of_a_denylisted_parent() {
        // #4031 review, CRITICAL 2: this module classifies text, never
        // expands a glob — `rm -rf /Users/bob/*` reached the denylist check
        // as the literal path `/Users/bob/*`, which matched no entry exactly,
        // while the shell's own expansion clears everything inside
        // `/Users/bob`. The glob's PARENT must be evaluated instead.
        let env = env_with_home("/Users/agent");
        let home = Path::new("/Users/agent");
        for (command, cwd) in [
            ("rm -rf /Users/someone/*", home),
            ("rm -rf ./*", home),
            ("rm -rf ~/*", home),
            ("rm -rf .[!.]*", home),
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, cwd, &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn allows_a_glob_inside_a_worktree_subdirectory() {
        // The companion allow case: a glob whose parent is an ORDINARY
        // subdirectory (not a denylisted root) is not this rule's business.
        let env = env_with_home("/Users/agent");
        let worktree = Path::new("/repo/.claude/worktrees/agent-x");
        assert_eq!(
            evaluate_destructive_delete_command_in("rm -rf ./target/*", worktree, &env),
            None
        );
    }

    #[test]
    fn denies_backslash_and_command_wrapper_bypasses() {
        // #4031 review, HIGH 3/4: `\rm` and `command rm` are the standard
        // POSIX alias-bypass idioms — both run the real `rm` exactly as
        // `rm` does, and both previously slipped past `first_command_token`
        // unresolved.
        let env = env_with_home("/Users/agent");
        for command in ["\\rm -rf /root", "command rm -rf /root"] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn denies_wrapper_words_regardless_of_enumeration() {
        // #4031 review pass 2, item 1: verb detection no longer depends on
        // enumerating wrapper words at all — `env -i` and `command -p` (both
        // followed by a FLAG, which `first_command_token`/`strip_wrapper_prefix`
        // conservatively refuse to resolve past) still deny here, because
        // this scans every token for the verb rather than resolving "the"
        // program.
        let env = env_with_home("/Users/agent");
        for command in [
            "env -i rm -rf /root",
            "command -p rm -rf /root",
            "nice rm -rf /root",
            "exec rm -rf /root",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn denies_bare_container_roots() {
        // #4031 review pass 2, item 3: a bare container (every user's home,
        // every mounted volume, system configuration) is as destructive to
        // delete whole as a single user's home root.
        let env = env_with_home("/Users/agent");
        for command in [
            "rm -rf /Users",
            "rm -rf /Users/*",
            "rm -rf /home",
            "rm -rf /Volumes",
            "rm -rf /etc",
            "rm -rf /var",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn denies_a_linux_users_home_root_like_its_macos_equivalent() {
        // #4031 review pass 3: `is_user_home_root` recognized only
        // `/Users/<name>` at first — `rm -rf /home/someoneelse` and its
        // glob-suffixed form denied on macOS but allowed on Linux, the same
        // hazard unenforced on half the platforms this guard runs on.
        let env = env_with_home("/Users/agent");
        for command in ["rm -rf /home/someoneelse", "rm -rf /home/someoneelse/*"] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                Some(DESTRUCTIVE_DELETE_REASON),
                "expected deny for: {command}"
            );
        }
    }

    #[test]
    fn allows_an_ordinary_project_under_a_linux_user_home() {
        // The companion allow case: a subdirectory INSIDE a Linux user's home
        // is ordinary work, exactly like `/Users/bob/Projects/foo` on macOS.
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "rm -rf /home/x/project/target",
                Path::new("/repo"),
                &env
            ),
            None
        );
    }

    #[test]
    fn denies_unresolvable_delete_targets() {
        // #4031 review pass 2, item 2: a delete verb with no resolvable
        // target — an unbalanced-quote segment mentioning one, or a bare verb
        // invocation — fails CLOSED rather than silently allowing.
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in("echo 'rm -rf /root", Path::new("/repo"), &env),
            Some(DESTRUCTIVE_DELETE_UNRESOLVED_REASON)
        );
        assert_eq!(
            evaluate_destructive_delete_command_in("rm --", Path::new("/repo"), &env),
            Some(DESTRUCTIVE_DELETE_UNRESOLVED_REASON)
        );
    }

    #[test]
    fn allows_over_matched_non_verb_mentions_that_resolve_to_no_target() {
        // The companion property to `denies_unresolvable_delete_targets`: an
        // unparseable segment that does NOT mention a delete verb at all is
        // simply not this rule's business, and a `find` with no `-delete`
        // action is a plain search, never denied regardless of its argument.
        let env = env_with_home("/Users/agent");
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "echo 'unrelated unterminated",
                Path::new("/repo"),
                &env
            ),
            None
        );
        assert_eq!(
            evaluate_destructive_delete_command_in(
                "find /repo -name '*.rs'",
                Path::new("/repo"),
                &env
            ),
            None
        );
    }

    #[test]
    fn allows_wrapped_non_destructive_commands() {
        // Companion allow cases: a wrapper preceding a NON-delete command, or
        // a delete verb whose target is genuinely benign, must stay allowed
        // — over-matching applies to the VERB, not to every wrapped command.
        let env = env_with_home("/Users/agent");
        for command in [
            "nice cargo clean",
            "time rm -rf ./target",
            "env FOO=1 rm stale.txt",
        ] {
            assert_eq!(
                evaluate_destructive_delete_command_in(command, Path::new("/repo"), &env),
                None,
                "expected allow for: {command}"
            );
        }
    }

    #[test]
    fn glob_parent_resolves_the_directory_a_glob_would_clear() {
        assert_eq!(glob_parent(Path::new("/repo/*")), Path::new("/repo"));
        assert_eq!(glob_parent(Path::new("/repo/a*b")), Path::new("/repo"));
        assert_eq!(
            glob_parent(Path::new("/repo/dir/*")),
            Path::new("/repo/dir")
        );
        assert_eq!(glob_parent(Path::new("/repo/.[!.]*")), Path::new("/repo"));
        // A bare `*` with no directory prefix yields `Path::parent`'s empty
        // relative path — unreachable in the real pipeline, since
        // `resolve_target_path` always joins a relative token onto the
        // tracked cwd first, but exercised here directly for completeness.
        assert_eq!(glob_parent(Path::new("*")), Path::new(""));
        // Non-glob paths are returned unchanged.
        assert_eq!(
            glob_parent(Path::new("/repo/file.txt")),
            Path::new("/repo/file.txt")
        );
    }
}
