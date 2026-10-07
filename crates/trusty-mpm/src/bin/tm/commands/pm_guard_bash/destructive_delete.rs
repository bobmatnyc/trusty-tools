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
//! rewrite closes. #8735: the shared `program_word` resolver is asked first,
//! so a path-spelled verb behind wrapper options (`nice -n 5 /bin/rm`) is
//! found, and a wrapper option it cannot read denies once a verb is in sight;
//! each `$(…)`, backtick, `<(…)` and `>(…)` body is judged as a command of
//! its own ([`classify_at_depth`]).
//! Once a verb token is found, its DELETION-TARGET argument(s) are resolved
//! against a `cd`-tracked effective working directory (the same [`PathEnv`]
//! expansion and lexical normalization [`super::evaluate_worktree_add_command`]
//! uses) and checked against [`denylisted_delete_class`], which also
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
//! - A delete verb hidden inside a file this module never opens is not
//!   detected: one segment WRITES a script containing `rm -rf /` (e.g.
//!   `python3 -c "open('/tmp/x.sh','w').write('rm -rf /')"`, or the same
//!   payload as a heredoc body) and a later segment merely EXECUTES that file
//!   (`bash /tmp/x.sh`) — neither segment's own tokens carry a delete verb, so
//!   per-segment scanning ([`split_shell_segments`]) finds nothing to deny.
//!   Pre-existing today via the one-line form above; accepted rather than
//!   closed (owner ruling, #7190) — closing it needs cross-segment tracking
//!   of "this path was just written, and a later segment executes it", which
//!   this module does not attempt. A heredoc-body allowlist redesign
//!   considered for #7190 does not add this class, only removes an
//!   incidental catch a masked multi-line body happened to trigger.
//!
//! Test: `denies_filesystem_root_deletion`, `denies_repo_root_deletion`,
//! `denies_dot_git_deletion`, `denies_worktree_root_deletion`,
//! `allows_worktree_interior_paths`, `allows_ordinary_cleanup`,
//! `denies_rmdir_of_a_denylisted_root`, `denies_find_delete_of_a_denylisted_root`,
//! `denies_a_delete_hidden_in_a_composed_command`,
//! `denies_home_expanded_from_a_literal_dollar_home`,
//! `denies_wrapper_words_regardless_of_enumeration`,
//! `denies_bare_container_roots`, `denies_unresolvable_delete_targets`,
//! `allows_over_matched_non_verb_mentions_that_resolve_to_no_target`,
//! `denies_a_delete_inside_a_substitution_body`,
//! `denies_a_delete_nested_past_the_depth_cap`,
//! `resolves_the_delete_verb_past_wrapper_options` below;
//! `pm_guard_denies_destructive_delete_of_repo_root` and siblings in
//! `tests/tm_hook_pm_guard.rs` exercise the end-to-end binary path.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use trusty_mpm::core::project_aliases::main_checkout_root;

use super::continuation;
use super::heredoc::{HeredocBodies, blank_spans, data_bodies};
use super::substitutions::{
    HEREDOC_RUNNERS, Substitution, blank_inert_heredocs, segment_substitutions,
    shell_run_heredoc_bodies,
};
use super::{
    MAX_WRAPPER_DEPTH, PathEnv, resolve_target_path, shell_lex, split_shell_segments,
    split_shell_segments_raw,
};
use crate::commands::hook_rewrite::first_command_token;
use crate::commands::program_word::resolve_program_word;

/// Deny reason for `rm`/`rmdir`/`unlink`/`find -delete` targeting a
/// denylisted destructive root (issue #4031).
///
/// Why: a bare refusal invites a retry with a different verb or a hand-rolled
/// workaround, so the text names every denylisted category and the one
/// sanctioned path for the worktree case (hand it back to the PM), the same way
/// `WORKTREE_REMOVE_DENY_REASON` does for the sibling `git worktree remove`
/// rule.
/// What: the `permissionDecisionReason` string emitted on this deny.
/// Test: `the_agent_side_worktree_denies_hand_back_and_never_name_a_force_sweep`.
// #8577: the worktree remedy named a fleet-wide `--force` sweep; it is now the
// single-tree hand-back.
pub(crate) const DESTRUCTIVE_DELETE_REASON: &str = "`rm`/`rmdir`/`unlink`/`find -delete` must \
     not target a filesystem root or bare container (`/`, `/root`, `/Users`, `/Users/<name>`, \
     `/home`, `/home/<name>`, `/Volumes`, `/private`, `/var`, `/etc`, `/usr`, `/opt`, `/Library`, \
     `/System`, `/Applications`, `$HOME`, or `$HOME`'s parent directory), a repository root, a `.git` \
     directory, or a `.claude/worktrees`/`.worktrees` entry (issue #4031) — each is either \
     unrecoverable data loss or another session's or workstream's uncommitted work. Ordinary file \
     and directory cleanup elsewhere (build artifacts, stale files, `git clean -fd`) is unaffected. \
     To remove a worktree, hand it back: report its path to the PM (or to `version-control`) \
     and stop. `rm -rf` on a worktree directory is never the workaround, and neither is a sweep \
     over other worktrees.";

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
/// What: delegates to [`classify_destructive_delete_in`] against the
/// guard process's real environment.
/// Test: see the module doc's test list.
pub(crate) fn evaluate_destructive_delete_command(
    command: &str,
    cwd: &Path,
) -> Option<DeleteTarget> {
    classify_destructive_delete_in(command, cwd, &PathEnv::from_process())
}

/// What a denied delete would destroy, least severe first (#8878).
///
/// Why: the #8878 hard floor keeps `rm -rf` of `$HOME` or `/` universal and
/// ahead of the bypasses, while D5 exempts the Architect from the worktree
/// rule, so one verdict must say which class a command hit — the most severe.
/// What: `Worktree` (a `.claude/worktrees`/`.worktrees` entry, D5),
/// `Repository` (a repository root or `.git`), `Root` (a filesystem root,
/// bare container or home — the floor) and `Unresolved` (fail closed, and
/// also the floor, since it could be `/`).
/// Test: `the_most_severe_delete_class_wins`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeleteTarget {
    Worktree,
    Repository,
    Root,
    Unresolved,
}

impl DeleteTarget {
    /// Whether this class is the #8878 hard floor, held under every bypass.
    pub(crate) fn is_floor(self) -> bool {
        self >= Self::Root
    }

    /// The deny reason for this class.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Unresolved => DESTRUCTIVE_DELETE_UNRESOLVED_REASON,
            _ => DESTRUCTIVE_DELETE_REASON,
        }
    }
}

/// [`evaluate_destructive_delete_command`] as a reason, for the unit tests.
#[cfg(test)]
fn evaluate_destructive_delete_command_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
) -> Option<&'static str> {
    classify_destructive_delete_in(command, cwd, env).map(DeleteTarget::reason)
}

/// [`evaluate_destructive_delete_command`] against an explicit environment —
/// see [`PathEnv`] for why production and tests must not share `std::env`
/// mutation. Every target is judged and the most severe class returned.
fn classify_destructive_delete_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
) -> Option<DeleteTarget> {
    // #7190: a lone stdin consumer of a quoted here-document reads its body
    // as data, so only its operator line is judged.
    let masked = lone_inert_heredoc(command);
    // #9344: the mask hides no root delete from the data-body floor.
    let belt = data_body_root_delete(command, &[cwd.to_path_buf()], env);
    let command = masked.as_deref().unwrap_or(command);
    classify_at_depth(command, cwd, env, 0, &Cell::new(0)).max(belt)
}

/// [`DeleteTarget::Root`] when a line of any here-document body in `command`
/// is a destructive-root command, whatever program reads the body (#9344);
/// [`DeleteTarget::Unresolved`] for a wrapper the resolver cannot measure.
///
/// Why: a body read as data is never judged as a command, and a reader can
/// run it after all — a `!`-alias, a gh extension, a `gpg.program` set in an
/// earlier call. Belt and braces under [`super::heredoc_line`]'s trust rule.
/// What: splits each body into lines, each line into shell segments, and
/// denies a segment whose program word, past wrappers, is a delete verb with
/// a target in the root class, judged from every directory in `cwds`. A verb
/// anywhere else in a sentence, a backticked delete, an unresolvable target
/// and a repository or worktree target do not count, so prose stays allowed.
/// #9344 round 2: a segment whose program word the resolver cannot name and
/// that names a delete verb is [`DeleteTarget::Unresolved`], as in
/// [`classify_at_depth`].
/// Test: `data_reader_tests::a_data_reader_body_holding_a_root_delete_is_denied_9344`,
/// `data_reader_tests::commit_and_pr_body_shapes_stay_allowed_9344`.
fn data_body_root_delete(command: &str, cwds: &[PathBuf], env: &PathEnv) -> Option<DeleteTarget> {
    let heredocs = HeredocBodies::scan(command);
    heredocs
        .bodies()
        .iter()
        .flat_map(|body| command[body.span.0..body.span.1].lines())
        .flat_map(split_shell_segments_raw)
        .filter_map(|segment| {
            let trimmed = segment.trim();
            let argv = shlex::split(trimmed).unwrap_or_else(|| {
                let words = trimmed.split_whitespace();
                words.map(|w| w.replace(['\'', '"'], "")).collect()
            });
            // #9344 round 2: an unresolvable wrapper fails closed.
            let Ok(word) = resolve_program_word(&argv) else {
                return segment_mentions_a_delete_verb(trimmed).then_some(DeleteTarget::Unresolved);
            };
            let verb = argv.get(word.index).map_or("", |w| verb_name(w));
            let root = !word.lookup
                && DELETE_VERBS.contains(&verb)
                && delete_targets(verb, &argv[word.index + 1..])
                    .iter()
                    .any(|target| {
                        cwds.iter().any(|dir| {
                            let path = resolve_target_path(target, dir, env);
                            is_root_class(glob_parent(&path), env)
                        })
                    });
            root.then_some(DeleteTarget::Root)
        })
        .max()
}

/// Programs that read a quoted here-document body on stdin as data (#7190).
const STDIN_DATA_CONSUMERS: &[&str] = &["python3", "python", "node", "ruby", "cat"];

/// `command` with its here-document body blanked, when the whole command is
/// one stdin consumer fed one quoted here-document; otherwise `None` (#7190).
///
/// Why: `python3 - <<'PY'` with `print('It\'s time to find it')` in its body
/// was refused: the body does not lex, and the fallback matched the word
/// `find`. The shell never runs a quoted body, so the verb is text. Owner
/// rulings on #7190 bound the mask: a body captured and re-run (`eval
/// $(cat <<'X'…)`, `read x <<'X'; eval "$x"`, `source /dev/stdin`, a pipe to
/// a shell) stays live, so the mask applies only where no capture can exist.
/// What: exactly one data here-document, its delimiter quoted and one that
/// [`super::heredoc::delimiter_word`] accepts (#9150), its operator line the
/// first line, and nothing after its terminator line. The operator
/// line carries no `|`, `;`, `&`, `$`, backtick, parenthesis or backslash, and
/// lexes to a [`STDIN_DATA_CONSUMERS`] program with no prefix assignment or
/// wrapper: an interpreter takes at most `-`, and `cat` at most one `>`/`>>`
/// redirect to a literal path. The body bytes become spaces, newlines kept.
/// Residual (accepted with the #7190 class-3 residual): an interpreter body
/// can still run a delete itself (`os.system`), as `python3 -c` and a lexable
/// body already can on the pre-change guard.
/// Test: `allows_a_quoted_heredoc_body_fed_to_an_interpreter_7190`,
/// `keeps_every_captured_or_shell_run_heredoc_body_live_7190`.
pub(crate) fn lone_inert_heredoc(command: &str) -> Option<String> {
    let bodies = data_bodies(command);
    let [body] = bodies.as_slice() else {
        return None;
    };
    let (line_start, line_end) = body.operator_line;
    if body.expands || !command[..line_start].trim().is_empty() {
        return None;
    }
    let line = &command[line_start..line_end];
    if line.contains(['|', ';', '&', '$', '`', '(', ')', '\\']) {
        return None;
    }
    let argv = shlex::split(line)?;
    let (program, rest) = argv.split_first()?;
    if !STDIN_DATA_CONSUMERS.contains(&program.as_str()) {
        return None;
    }
    let (heredocs, others): (Vec<&String>, Vec<&String>) =
        rest.iter().partition(|t| t.starts_with("<<"));
    if heredocs.len() != 1 || heredocs[0].starts_with("<<<") {
        return None;
    }
    // #7190: the terminator line is all that may follow the body. A delimiter
    // the shell reads whole (`<<'A B'`) never gets here: #9150's scan refuses
    // it and claims no body.
    let word = &heredocs[0][2..];
    let word = word.strip_prefix('-').unwrap_or(word);
    if command[body.span.1..].trim() != word {
        return None;
    }
    let plain =
        |path: &str| !path.is_empty() && !path.contains(['*', '?', '[', ']', '{', '}', '~']);
    let consumes = match (program.as_str(), others.as_slice()) {
        (_, []) => true,
        ("cat", [op, path]) => matches!(op.as_str(), ">" | ">>") && plain(path),
        ("cat", [glued]) => glued
            .strip_prefix(">>")
            .or_else(|| glued.strip_prefix('>'))
            .is_some_and(plain),
        ("cat", _) => false,
        (_, [dash]) => *dash == "-",
        _ => false,
    };
    consumes.then(|| blank_spans(command, &[body.span]))
}

/// Calls to [`classify_at_depth`] one command may make before it denies as
/// unresolved (#8735 round 2): each split body is judged from every working
/// directory its level saw, so nested bodies multiply the work.
const MAX_DELETE_WORK: usize = 4096;

/// [`classify_destructive_delete_in`] for text nested `depth` substitutions
/// deep (#8735).
///
/// Why: the lexer returns `$(rm -rf /)` as one word, so a delete inside a
/// command substitution, a backtick or a process substitution never reached
/// the verb scan (`echo "$(rm -rf /)"`).
/// What: each segment's substitution bodies are judged, as commands of their
/// own, from the segment's working directory. The substitution scan skips a
/// here-document body that is stdin text, but judges what an unquoted-delimiter
/// body runs ([`blank_inert_heredocs`], #8735 round 2); the verb scan still
/// reads the body, since a captured body can run again (`eval $(cat <<'X'…)`).
/// A body a separator split across segments is judged whole afterwards from
/// every working directory the segments saw, and the most severe class kept
/// (#8735 round 2). #9155: each here-document body a shell runs, and each
/// string a `bash -c`-style wrapper runs, is judged the same way as a whole
/// command ([`nested_commands`]), which reads a here-document nested in it.
/// Past [`MAX_WRAPPER_DEPTH`]
/// levels the text is not read further, and a delete verb anywhere in it
/// denies as unresolved; past [`MAX_DELETE_WORK`] calls, anything does.
/// #9180: each level also judges its continuation-joined spelling
/// ([`continuation::joined`]), and a level whose here-document scan lost
/// confidence denies as unresolved once a delete verb is in sight.
/// Test: `denies_a_delete_inside_a_substitution_body`,
/// `continuation_tests::the_floor_denies_each_9180_class`,
/// `continuation_tests::the_floor_fails_closed_on_an_unplaceable_heredoc_9180`,
/// `denies_a_heredoc_nested_in_a_shell_run_body_or_wrapper_9155`,
/// `denies_a_delete_nested_past_the_depth_cap`,
/// `allows_a_scratch_delete_inside_a_substitution`,
/// `judges_a_split_body_from_every_directory_seen`,
/// `allows_a_backtick_in_a_quoted_heredoc_body`.
fn classify_at_depth(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
    depth: usize,
    work: &Cell<usize>,
) -> Option<DeleteTarget> {
    work.set(work.get() + 1);
    if work.get() > MAX_DELETE_WORK {
        return Some(DeleteTarget::Unresolved);
    }
    // #8735: past the cap nothing is read; a delete verb in sight denies.
    if depth > MAX_WRAPPER_DEPTH {
        return segment_mentions_a_delete_verb(command).then_some(DeleteTarget::Unresolved);
    }
    // #9180: a here-document the scan gave up on leaves which lines run
    // unknown, so a delete verb in sight fails closed.
    if HeredocBodies::scan(command).lost_confidence() && segment_mentions_a_delete_verb(command) {
        return Some(DeleteTarget::Unresolved);
    }
    // #9180: the continuation-joined spelling is judged too; it only adds.
    let mut worst = continuation::joined(command)
        .and_then(|joined| classify_at_depth(&joined, cwd, env, depth, work));
    let mut effective_cwd = cwd.to_path_buf();
    let mut cwds_seen: Vec<PathBuf> = vec![effective_cwd.clone()];
    let mut judged: HashSet<String> = HashSet::new();
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        // #8735: each `$(…)`, backtick, `<(…)` and `>(…)` body runs on its own.
        for body in substitution_bodies(trimmed) {
            let inner = classify_at_depth(body.text(), &effective_cwd, env, depth + 1, work);
            worst = worst.max(inner);
            judged.insert(body.text().to_string());
        }
        // Same `cd`-tracking shape as `evaluate_worktree_add_command_in`: a
        // deliberate, partial closing of `cd /tmp && rm -rf x` — see that
        // function's doc for the residual (shell-variable / substitution
        // built `cd` target) this shares. Unlike verb detection below, this
        // still goes through `first_command_token` — a wrapped `cd` (`nice cd
        // /tmp`) is a real but narrower gap than wrapper-enumerated VERB
        // detection was, since a missed `cd` only leaves `effective_cwd`
        // stale rather than letting a delete verb through unclassified.
        if first_command_token(trimmed).as_deref() == Some("cd") {
            if let Some(argv) = shlex::split(trimmed)
                && let Some(dest) = argv.get(1)
            {
                effective_cwd = resolve_target_path(dest, &effective_cwd, env);
                if !cwds_seen.contains(&effective_cwd) {
                    cwds_seen.push(effective_cwd.clone());
                }
            }
            continue;
        }
        let Some(argv) = shlex::split(trimmed) else {
            // Unbalanced quotes — cannot tokenize this segment at all. Fail
            // CLOSED (item 2) only when the raw text plausibly names one of
            // the delete verbs as a whole word; an unparseable segment with
            // nothing suspicious in it is simply not this rule's business.
            if segment_mentions_a_delete_verb(trimmed) {
                return Some(DeleteTarget::Unresolved);
            }
            continue;
        };
        // #8735: the shared resolver names the program past wrappers and
        // their options; one it cannot resolve denies once a verb is in sight.
        let resolved = resolve_program_word(&argv);
        if resolved.is_err() && segment_mentions_a_delete_verb(trimmed) {
            return Some(DeleteTarget::Unresolved);
        }
        // #8735 round 2: `command -v rm` and `sudo -l rm` run nothing.
        if resolved.is_ok_and(|w| w.lookup) {
            continue;
        }
        let program_at = resolved.ok().map(|w| w.index).filter(|&at| {
            argv.get(at)
                .is_some_and(|w| DELETE_VERBS.contains(&verb_name(w)))
        });
        // #4031 review, item 1: otherwise scan EVERY token for a delete verb —
        // see the module doc for why. A leading `\` is stripped before
        // comparison (the same alias-bypass idiom the resolver strips).
        let Some(verb_idx) = program_at.or_else(|| {
            argv.iter()
                .position(|tok| DELETE_VERBS.contains(&tok.strip_prefix('\\').unwrap_or(tok)))
        }) else {
            continue;
        };
        let verb = verb_name(&argv[verb_idx]);
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
            return Some(DeleteTarget::Unresolved);
        }
        let repo_root = main_checkout_root(&effective_cwd);
        for target in targets {
            let resolved = resolve_target_path(&target, &effective_cwd, env);
            worst = worst.max(denylisted_delete_class(
                &resolved,
                repo_root.as_deref(),
                env,
            ));
        }
    }
    // #8735: an unquoted `;`, `&&` or `|` inside a body splits it across
    // segments (`x=$(true; rm -rf /)`), so a body not seen whole is judged here.
    // Round 2: from every directory seen (`cd / && x=$(true; rm -rf Users)`),
    // since which segment's directory it runs in is not known.
    for body in substitution_bodies(command) {
        if !judged.insert(body.text().to_string()) {
            continue;
        }
        for dir in &cwds_seen {
            worst = worst.max(classify_at_depth(body.text(), dir, env, depth + 1, work));
        }
    }
    // #9344: a root delete in any here-document body, data or not.
    worst = worst.max(data_body_root_delete(command, &cwds_seen, env));
    // #9155: split at its newlines, a nested here-document was never whole.
    for inner in nested_commands(command) {
        if !judged.insert(inner.clone()) {
            continue;
        }
        for dir in &cwds_seen {
            worst = worst.max(classify_at_depth(&inner, dir, env, depth + 1, work));
        }
    }
    worst
}

/// The commands `command` runs as text of their own: each here-document body
/// a shell may run (#9180: any but a data reader's), and the inner string of each
/// top-level `sh -c` / `bash -c` / `eval` / `xargs` wrapper (#9155).
///
/// Why: the segment splitter cuts both at every newline, so `bash <<'O'`
/// around `bash <<I` around `echo '$(rm -rf /)'` reached the floor as lines,
/// and the inner unquoted here-document — whose single-quoted substitution
/// bash expands — was never recognised.
/// What: [`shell_run_heredoc_bodies`], then [`shell_lex::wrapped_command`] of
/// each top-level segment. A deeper wrapper is reached by the recursion.
/// Test: `denies_a_heredoc_nested_in_a_shell_run_body_or_wrapper_9155`.
fn nested_commands(command: &str) -> Vec<String> {
    let mut found = shell_run_heredoc_bodies(command);
    for raw in split_shell_segments_raw(command) {
        if let shell_lex::WrappedCommand::Inner(inner) = shell_lex::wrapped_command(raw.trim()) {
            found.push(inner);
        }
    }
    found
}

/// The substitution bodies `text` runs: those of its argv text with each
/// stdin-text here-document body blanked, then those every unquoted-delimiter
/// body expands, quotes read as literal text (#8735 round 2 — a backtick in
/// `git commit -F - <<'EOF'` is text; #9155 — `python3 - <<PY` with
/// `print('$(rm -rf /)')` runs the `rm`). A body found twice is listed once.
/// Test: `allows_a_backtick_in_a_quoted_heredoc_body`,
/// `denies_a_single_quoted_substitution_in_an_expanding_body_9155`.
fn substitution_bodies(text: &str) -> Vec<Substitution> {
    let (argv_text, expanded) = blank_inert_heredocs(text, HEREDOC_RUNNERS);
    let mut bodies = segment_substitutions(&argv_text);
    for body in expanded {
        if !bodies.contains(&body) {
            bodies.push(body);
        }
    }
    bodies
}

/// A word's program name: a leading `\` and any directory dropped (#8735).
fn verb_name(word: &str) -> &str {
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word)
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

/// Which destructive-root category issue #4031 denylists `path` falls in,
/// if any.
///
/// What: a class when `path` — after [`resolve_target_path`]'s expansion and
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
// #8878: returns the class, so the floor can tell `$HOME` from a worktree.
fn denylisted_delete_class(
    path: &Path,
    repo_root: Option<&Path>,
    env: &PathEnv,
) -> Option<DeleteTarget> {
    let path = glob_parent(path);
    if is_root_class(path, env) {
        return Some(DeleteTarget::Root);
    }
    if path.file_name().and_then(|f| f.to_str()) == Some(".git")
        || repo_root.is_some_and(|root| path == root)
    {
        return Some(DeleteTarget::Repository);
    }
    is_worktree_root_or_container(path).then_some(DeleteTarget::Worktree)
}

/// Whether `path` is a filesystem root, a bare container, or a home directory.
fn is_root_class(path: &Path, env: &PathEnv) -> bool {
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
    is_user_home_root(path)
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
/// `rm -rf /Users/bob/*` reached [`denylisted_delete_class`] as the
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

    /// #8878: a worktree target listed first must not hide `$HOME` after it.
    #[test]
    fn the_most_severe_delete_class_wins() {
        let env = env_with_home("/Users/agent");
        let class = |command: &str| classify_destructive_delete_in(command, Path::new("/w"), &env);
        let wt = "/w/.claude/worktrees/a";
        assert_eq!(class(&format!("rm -rf {wt}")), Some(DeleteTarget::Worktree));
        assert_eq!(
            class(&format!("rm -rf {wt} && rm -rf $HOME")),
            Some(DeleteTarget::Root)
        );
        assert_eq!(class(&format!("rm -rf {wt} /")), Some(DeleteTarget::Root));
        assert_eq!(class("rm -rf /w/x/.git"), Some(DeleteTarget::Repository));
        assert_eq!(class("rm -f"), Some(DeleteTarget::Unresolved));
        assert!(DeleteTarget::Root.is_floor() && DeleteTarget::Unresolved.is_floor());
        assert!(!DeleteTarget::Worktree.is_floor() && !DeleteTarget::Repository.is_floor());
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

    /// The class `command` gets, run from `/repo` with `$HOME=/Users/agent`.
    fn class_of(command: &str) -> Option<DeleteTarget> {
        classify_destructive_delete_in(command, Path::new("/repo"), &env_with_home("/Users/agent"))
    }

    /// #8735: every row was allowed on main (bd712bcfa6).
    #[test]
    fn denies_a_delete_inside_a_substitution_body() {
        for command in [
            "echo \"$(rm -rf /)\"",
            "echo `rm -rf ~`",
            "x=$(rm -rf \"$HOME\")",
            "echo \"$(echo $(rm -rf /))\"",
            "cat <(rm -rf /root)",
            "echo x > >(rm -rf /)",
            "cd /tmp && echo \"$(rm -rf /Users/agent)\"",
            "x=$(true; rm -rf /)",
            "echo \"$(true && rm -rf ~)\"",
        ] {
            assert!(
                class_of(command).is_some_and(DeleteTarget::is_floor),
                "{command}"
            );
        }
    }

    /// #8735: a scratch delete in a body is judged by its target, as it is
    /// outside one.
    #[test]
    fn allows_a_scratch_delete_inside_a_substitution() {
        for command in [
            "x=$(rm -f /tmp/scratch/file)",
            "echo \"$(git rev-parse HEAD)\"",
            "echo `date` \"$(rm -f stale.txt)\"",
        ] {
            assert_eq!(class_of(command), None, "{command}");
        }
    }

    /// #8735: nine nested bodies are one past [`MAX_WRAPPER_DEPTH`]; eight are
    /// read to the bottom.
    #[test]
    fn denies_a_delete_nested_past_the_depth_cap() {
        let nest = |levels: usize| {
            let mut text = "rm -f /tmp/scratch/file".to_string();
            for _ in 0..levels {
                text = format!("echo $({text})");
            }
            text
        };
        assert_eq!(class_of(&nest(8)), None);
        assert_eq!(class_of(&nest(9)), Some(DeleteTarget::Unresolved));
    }

    /// #8735: the shared resolver names a path-spelled verb behind wrapper
    /// options, and a wrapper option it cannot measure denies.
    #[test]
    fn resolves_the_delete_verb_past_wrapper_options() {
        assert_eq!(
            class_of("nice -n 5 /bin/rm -rf /"),
            Some(DeleteTarget::Root)
        );
        assert_eq!(
            class_of("timeout 5 /bin/rm -rf /root"),
            Some(DeleteTarget::Root)
        );
        assert_eq!(
            class_of("sudo --bogus rm -f /tmp/x"),
            Some(DeleteTarget::Unresolved)
        );
        assert_eq!(class_of("timeout 30 cargo test -p x"), None);
        assert_eq!(class_of("nice -n 10 cargo build"), None);
    }

    /// #8735 round 2: a wrapper and its operand in front of a `-c` string hid
    /// the string from `wrapped_command`. The first two rows were allowed at
    /// 7cb9de9271; all fail against the fail-open mutation of the resolved arm.
    #[test]
    fn denies_a_delete_in_a_command_string_behind_a_wrapper() {
        for command in [
            "timeout 60 bash -c 'rm -rf ~'",
            "nice -n 5 sh -c 'rm -rf /'",
            "timeout 5 env -S 'rm -rf /'",
            "flock /tmp/lock -c 'rm -rf ~'",
        ] {
            assert!(
                class_of(command).is_some_and(DeleteTarget::is_floor),
                "{command}"
            );
        }
    }

    /// #8735 round 2: a body split across segments ran in the directory its
    /// segment reached, not the starting one. The deny rows were allowed at
    /// 7cb9de9271.
    #[test]
    fn judges_a_split_body_from_every_directory_seen() {
        for command in [
            "cd / && x=$(true; rm -rf Users)",
            "cd /; x=$(true; rm -rf Users)",
            "cd /Users && y=$(true; rm -rf agent)",
        ] {
            assert_eq!(class_of(command), Some(DeleteTarget::Root), "{command}");
        }
        assert_eq!(class_of("cd /tmp && x=$(true; rm -rf scratch)"), None);
    }

    /// #8735 round 2: the fan-out over directories is bounded; past
    /// [`MAX_DELETE_WORK`] calls the command denies as unresolved.
    #[test]
    fn denies_when_the_split_body_work_runs_out() {
        let nest = |levels: usize| {
            let cds: String = (0..9).map(|n| format!("cd /tmp/d{n}; ")).collect();
            let mut text = "rm -f scratch".to_string();
            for _ in 0..levels {
                text = format!("{cds}x=$(true; {text})");
            }
            text
        };
        assert_eq!(class_of(&nest(2)), None);
        assert_eq!(class_of(&nest(5)), Some(DeleteTarget::Unresolved));
    }

    /// #8735 round 2: a quoted-delimiter here-document body is text, so a
    /// backtick in a commit message or PR body is not a command. Both allow
    /// rows were denied at 7cb9de9271. A live substitution in an expanding
    /// body, and a body a shell, `sudo` or `xargs` reads, still deny.
    #[test]
    fn allows_a_backtick_in_a_quoted_heredoc_body() {
        for command in [
            "git commit -F - <<'EOF'\nfloor denies `rm -rf ~`\nEOF",
            "gh pr create --title t --body \"$(cat <<'EOF'\nthe floor denies `rm -rf ~`\nEOF\n)\"",
        ] {
            assert_eq!(class_of(command), None, "{command}");
        }
        for command in [
            "cat <<EOF\n$(rm -rf /)\nEOF",
            "cat <<EOF\n`rm -rf ~`\nEOF",
            "bash <<'EOF'\nrm -rf ~\nEOF",
            "sudo -s <<'EOF'\nrm -rf ~\nEOF",
            "xargs rm -rf <<'EOF'\n/\nEOF",
        ] {
            assert!(
                class_of(command).is_some_and(DeleteTarget::is_floor),
                "{command}"
            );
        }
    }

    /// #9155: an unquoted-delimiter body is expanded by the shell before any
    /// program reads it, so a single-quoted substitution in a body an
    /// interpreter or shell runs still reaches the floor. Each deny row was
    /// allowed at dcd33f9591. A quoted delimiter, or a body with no
    /// substitution, still allows.
    #[test]
    fn denies_a_single_quoted_substitution_in_an_expanding_body_9155() {
        for command in [
            "python3 - <<PY\nprint('$(rm -rf /)')\nPY",
            "bash <<X\necho '$(rm -rf /)'\nX",
            "python3 - <<PY\nprint('`rm -rf /`')\nPY",
            "node - <<JS\nconsole.log('$(rm -rf /)')\nJS",
            "bash <<X\ncat <<'Y'\n'$(rm -rf ~)'\nY\nX",
        ] {
            assert!(
                class_of(command).is_some_and(DeleteTarget::is_floor),
                "{command}"
            );
        }
        for command in [
            "python3 - <<'PY'\nprint('$(rm -rf /)')\nPY",
            "bash <<'X'\necho '$(rm -rf /)'\nX",
            "python3 - <<PY\nprint('it is rm -rf / in prose')\nPY",
            "python3 - <<PY\nprint('hello')\nPY",
            "python3 - <<PY\nprint('\\$(rm -rf /)')\nPY",
        ] {
            assert_eq!(class_of(command), None, "{command}");
        }
    }

    /// #9155: an unquoted here-document nested in a body a shell runs, or in
    /// a wrapper's string, is read whole, so its single-quoted substitution
    /// expands. A benign nested body allows; nesting past the depth cap with
    /// a delete verb in sight denies as unresolved.
    #[test]
    fn denies_a_heredoc_nested_in_a_shell_run_body_or_wrapper_9155() {
        for command in [
            "bash <<'O'\nbash <<I\necho '$(rm -rf /)'\nI\nO",
            "cat <<'O' | bash\nbash <<I\necho '$(rm -rf /)'\nI\nO",
            "sudo -s <<'O'\ncat <<I\n'$(rm -rf /)'\nI\nO",
            "bash -c \"bash <<I\necho '\\$(rm -rf /)'\nI\"",
            "bash -c \"cat <<I\n'\\$(rm -rf /)'\nI\"",
            // The outer shell turns `\$(` into `$(` before the inner one reads it.
            "bash <<O\nbash <<I\necho '\\$(rm -rf /)'\nI\nO",
        ] {
            assert!(
                class_of(command).is_some_and(DeleteTarget::is_floor),
                "{command}"
            );
        }
        for command in [
            "bash <<'O'\nbash <<I\necho '$(date)'\nI\nO",
            "bash -c \"cat <<I\nhello\nI\"",
            "bash <<'O'\nbash <<'I'\necho '$(rm -rf /)'\nI\nO",
        ] {
            assert_eq!(class_of(command), None, "{command}");
        }
        let nest = |levels: usize| {
            let mut text = "cat <<I\n'$(rm -rf /)'\nI".to_string();
            for level in 0..levels {
                text = format!("bash <<'L{level}'\n{text}\nL{level}");
            }
            text
        };
        assert_eq!(class_of(&nest(3)), Some(DeleteTarget::Root));
        assert_eq!(class_of(&nest(12)), Some(DeleteTarget::Unresolved));
    }

    /// #8735 round 2: a lookup runs nothing, and `sudo -k` and BSD `xargs -J`
    /// are grammar the resolver reads. Each row was denied at 7cb9de9271.
    #[test]
    fn allows_a_lookup_and_the_bsd_xargs_options() {
        for command in [
            "command -v rm",
            "sudo -l rm -rf /",
            "sudo -k rm -f x",
            "xargs -J % rm %",
        ] {
            assert_eq!(class_of(command), None, "{command}");
        }
        assert_eq!(class_of("sudo -k rm -rf /"), Some(DeleteTarget::Root));
    }

    /// #8735 round 2: behind an option the resolver cannot measure, the first
    /// word carrying a command string stands in. A quoted verb (`r""m`) hides
    /// from the raw-text verb check, so only that scan reads it. Allowed at
    /// 7cb9de9271, and under the fail-open mutation of the unresolved scan.
    #[test]
    fn denies_a_delete_in_a_command_string_behind_an_unknown_option() {
        for command in [
            "sudo --bogus bash -c 'r\"\"m -rf ~'",
            "timeout 5 env -S 'r\"\"m -rf /'",
        ] {
            assert_eq!(class_of(command), Some(DeleteTarget::Root), "{command}");
        }
    }

    /// 🔴 REGRESSION (#7190): a quoted here-document body fed to an
    /// interpreter or `cat` is data, so a delete verb in a string that does
    /// not lex is no command. Denied (unresolved) on origin/main.
    #[test]
    fn allows_a_quoted_heredoc_body_fed_to_an_interpreter_7190() {
        for command in [
            "python3 - <<'PY'\nprint('It\\'s time to find it')\nPY",
            "python3 <<\"PY\"\nprint('it's time to find it')\nPY",
            "node - <<'JS'\nconsole.log('can't find rm')\nJS\n",
            "cat > /tmp/s/x.py <<'EOF'\nassert x, 'it's not found: find it'\nEOF",
            "cat >>notes.py <<-'EOF'\n\tprint('won't unlink it')\n\tEOF",
        ] {
            assert_eq!(class_of(command), None, "{command}");
        }
    }

    /// #7190 bound (owner rulings 2026-09-14/15): every body a shell runs, a
    /// capture re-runs, an expansion reaches or a later segment can use keeps
    /// the scan, and so does any operator line the mask cannot read whole.
    #[test]
    fn keeps_every_captured_or_shell_run_heredoc_body_live_7190() {
        for command in [
            "eval $(cat <<'PY'\nrm -rf /\nPY\n)",
            "eval \"$(cat <<'PY'\nrm -rf /\nPY\n)\"",
            "bash <<'EOF'\nrm -rf /\nEOF",
            "read x <<'PY'\nrm -rf /\nPY\neval \"$x\"",
            "source /dev/stdin <<'PY'\nrm -rf /\nPY",
            "cat <<'PY' | \"bash\"\nrm -rf /\nPY",
            "cat <<'PY' > x.sh\nrm -rf /\nPY\nsh x.sh",
            "mapfile -t a <<'PY'\nrm -rf /\nPY\n\"${a[@]}\"",
            "python3 - <<PY\n$(rm -rf /)\nPY",
            "X=1 python3 - <<'PY'\nprint('it's find')\nPY",
            "python3 -c x <<'PY'\nprint('it's find')\nPY",
            "python3 - <<'PY' 2>/dev/null\nprint('it's find')\nPY",
            "sudo python3 - <<'PY'\nprint('it's find')\nPY",
            "cat > $OUT <<'PY'\nprint('it's find')\nPY",
            "cat <<'A' <<'B'\nprint('it's find')\nA\nx\nB",
            "rm -rf / ; python3 - <<'PY'\nprint('it's find')\nPY",
        ] {
            assert!(class_of(command).is_some(), "{command}");
        }
        // Red-team round: bash and zsh keep a quoted delimiter whole (`A B`,
        // `E>F`), so the delete after that terminator runs. Allowed at a30ff02fbc;
        // #9150's scan now refuses the delimiter, so no mask applies.
        for command in [
            "cat <<'A B'\nhello\nA B\nrm -rf /\nA",
            "python3 - <<\"E>F\"\nprint(1)\nE>F\nrm -rf ~\nE",
            "cat > x.py <<'E<F'\nx\nE<F\nrm -rf $HOME\nE",
        ] {
            assert_eq!(class_of(command), Some(DeleteTarget::Root), "{command}");
        }
    }
}
