//! Bash-command classification for the `tm hook --pm-guard` PreToolUse guard
//! (issue #1977, hardened in PR #1985; sed/awk deny-by-default redesign #2664).
//!
//! Why: Bash is the escape hatch a prompt-only prohibition can't close — a PM
//! can edit a file with `sed -i`, write one with `echo … > f.rs`, apply a diff
//! with `git apply`, or run the suite with `pytest`, all of which side-step the
//! P1–P5 prohibitions. Worse, shell *composition* (`&&`, `||`, `;`, `|`, a bare
//! `&`, a newline) and *substitution* — command (`$(…)`, backticks) and
//! process (`<(…)`, `>(…)`, #2745) alike — are themselves bypasses: a benign
//! leading verb can hide a forbidden one further down the line or inside a
//! substitution body. This module is the quote-unaware,
//! conservatively-over-blocking classifier that closes those seams; it is
//! factored out of `pm_guard.rs` so that file stays under the 500-SLOC cap.
//! What: [`evaluate_bash_command`] splits a command on every composition
//! separator, classifies each segment (first-token verb, two-token
//! `git apply` / `npm test`, and recursively every substitution body), and
//! applies a whole-command file-write redirection check. A missed deny is the
//! dangerous direction, so ambiguous forms (unbalanced substitutions,
//! over-deep nesting) deny. `sed`/`awk`-family verbs are classified by the
//! sibling [`sed_awk`] module, which is deny-by-default: a segment must prove
//! it is narrowly read-only (no in-place flag, no external script load, no
//! write/exec script construct, balanced quotes) to be allowed. The sibling
//! [`persistence`] module runs the one ALLOW-list here — the agent-cost stop's
//! escape hatch (#4837) — and is default-deny in the opposite direction. The
//! sibling [`main_checkout`] module carries the rules `pm_guard` calls
//! directly, ahead of the subagent exemptions: whole-tree-destructive git
//! verbs aimed at a project's main checkout (ADR-0037), `git commit` there,
//! and the HEAD-moving `pull`/`merge`/`rebase` (ADR-0048). The sibling
//! [`heredoc`] module tells the redirection check which bytes are
//! here-document body content, so a `>` inside a `<<'PY'` script is a
//! comparison rather than a file write (#5356). The sibling
//! [`destructive_delete`] module carries the rule `pm_guard` calls directly,
//! ahead of the subagent exemptions, alongside the worktree-add-tmp guard: a
//! target-path denylist for `rm`/`rmdir`/`unlink`/`find … -delete` aimed at a
//! filesystem root, a repository root, a `.git` directory, or a worktree
//! entry (issue #4031). The sibling [`secret_file_copy`] module carries the
//! same ahead-of-subagent-exemptions rule for `cp`/`mv`: a source-basename
//! denylist (`*.tfvars`, `.env*`, `*.pem`, `*.key`, `id_*`,
//! `credentials`/`secrets` names) refused when the destination resolves into
//! a harness worktree (issue #7122). The sibling [`path_tokens`] module turns a path TOKEN
//! a command wrote into the directory every one of those rules decides on, and
//! reports what its expansion could not reach — the question a rule must ask
//! before stating anything about the directory it got back (#7098, #7100).
//! Test: `evaluate_bash_command_*`, `split_shell_segments_*`, and
//! `has_file_write_redirection_*` in this module's `tests` submodule;
//! `sed_awk::tests` for the sed/awk-specific safety analysis.

mod destructive_delete;
// #7497: the disk-usage half of the worktree-add gate, beside the temp-root
// half it shares a target resolver with.
mod disk_usage;
mod heredoc;
mod main_checkout;
mod path_tokens;
mod persistence;
mod secret_file_copy;
mod sed_awk;
mod shell_lex;
mod worktree_remove;
mod worktree_remove_rechecks;

pub(crate) use destructive_delete::evaluate_destructive_delete_command;
pub(crate) use main_checkout::{
    CommitVerdict, docs_commit_deny_reason, evaluate_main_checkout_commit_command,
    evaluate_main_checkout_destructive_command, head_move_deny_reason, main_checkout_head_move,
};
pub(crate) use persistence::command_is_persistence_only;
// #7266: the secret-read guard frames here-document bodies through the SAME
// scan the write-redirection check uses, rather than growing a second parser.
pub(crate) use heredoc::split_heredoc_bodies;
// #7266: everything after the first export is shared with
// `crate::commands::pm_guard_secret_read`, so a READ of a secret-bearing file
// is screened against the same pattern list, the same brace expander and the
// same process-substitution stripper a COPY of one is.
pub(crate) use secret_file_copy::{
    any_pattern_overlaps, evaluate_secret_file_copy_command, expand_brace_alternatives,
    matches_only_name_substring_family, secret_pattern_overlaps, strip_process_substitution,
};
// #7266 round 5: the read rule allowlists `git add`/`rm`/`mv`/`status`, and the
// subcommand behind `git -C <path> …` is already parsed here. One parser, two
// callers, rather than a second global-option table.
pub(crate) use shell_lex::git_subcommand;
// #5791: worktree removal is PM-executed, so an agent's `git worktree remove`
// denies. The sibling `worktree add` guard above is a different rule with a
// different scope — that one is about WHERE a tree is provisioned, this one is
// about WHO may destroy one.
// See ADR-0057 — one role's removal is now a grant, so the rule reports three
// answers rather than two and the third must still be earned.
pub(crate) use worktree_remove::{
    DispatchIdentity, WorktreeRemoveVerdict, evaluate_worktree_remove_command,
};
pub(crate) use worktree_remove_rechecks::evaluate_removal_rechecks;

use std::path::{Path, PathBuf};

use crate::commands::hook_rewrite::{effective_tool_name, first_command_token};
// Reached by the sibling rule modules through `super::…` — one answer to "what
// did not expand" (#7098, #7100), covering a surviving `~` since #7234.
use path_tokens::unresolved_target;
// One definition of "which directory does this token name", crate-visible since
// #7172: the `EnterWorktree` rule is no Bash rule but asks the same question, so
// it reaches this resolver instead of growing a second normalizer.
pub(crate) use path_tokens::{PathEnv, resolve_target_path};
use shell_lex::QuoteScan;

/// Deny reason for editing files through a shell tool (sed/awk/patch/git apply/redirection).
pub(crate) const SHELL_EDIT_REASON: &str = "PM must not edit files via shell tools \
     (sed/awk/patch/git apply/redirection) (prohibitions P1/P5). \
     Delegate the change to rust-engineer via the Task/Agent tool.";

/// Deny reason for running builds/tests directly (make/pytest/npm test).
pub(crate) const BUILD_TEST_REASON: &str = "PM must not run builds or tests directly \
     (prohibitions P4–P5). Delegate to rust-engineer / QA via the Task/Agent tool.";

/// Deny reason for fetching over the network from Bash (curl/wget).
pub(crate) const NETWORK_REASON: &str = "PM must not fetch over the network from Bash \
     (prohibition P-network). Use WebFetch/WebSearch or delegate the task.";

/// Deny reason for a wrapper whose inner command cannot be lexed (#6660).
pub(crate) const UNLEXABLE_WRAPPER_REASON: &str = "this command's `sh -c`/`bash -c`/`env -S`/\
     `xargs` wrapper carries an inner command with unbalanced quoting, so the guard cannot \
     classify what would actually run. Run the command without the wrapper, or fix the quoting.";

/// Deny reason for `$'…'`/`$"…"` quoting the guard cannot decode (#6660 review).
pub(crate) const ANSI_C_QUOTING_REASON: &str = "this command uses `$'…'`/`$\"…\"` quoting, which \
     the guard's lexer cannot decode — it cannot establish which program would actually run \
     (`sh -c $'git worktree remove x'` reads as `$git`, matching no rule). Rewrite the command \
     with ordinary `'…'` or `\"…\"` quoting.";

/// Deny reason for a wrapper nested past [`MAX_WRAPPER_DEPTH`] (#6660 review).
pub(crate) const WRAPPER_DEPTH_REASON: &str = "this command nests `sh -c`/`bash -c`/`xargs` \
     wrappers deeper than the guard will follow, so it cannot classify what would actually run. \
     Run the command without the nesting.";

/// How many wrapper layers the guard descends through (#6660).
///
/// Why: `sh -c "bash -c '…'"` is a real shape, and an adversarial one nests
/// without bound. The cap bounds the recursion; exhausting it DENIES rather
/// than falling through, matching [`MAX_SUBSTITUTION_DEPTH`] — the first cut
/// stopped recursing instead, which made a 9-layer wrapper an allow while an
/// 8-layer one denied.
/// What: the budget threaded through [`unclassifiable_command`] and
/// [`expand_shell_segments`].
const MAX_WRAPPER_DEPTH: usize = 8;

/// Whether the guard is unable to establish what `command` would run (#6660).
///
/// Why: the three ABSOLUTE guards a dispatched subagent actually hits —
/// `evaluate_worktree_remove_command`, the main-checkout destructive/commit/
/// HEAD-move rules, and `evaluate_destructive_delete_command` — are reached
/// from `pm_guard` BEFORE `payload_is_subagent_dispatch` short-circuits, and
/// none of them routes through [`evaluate_bash_command`]. A fail-closed check
/// that lived only in [`classify_bash_segment`] was therefore unreachable for
/// exactly the caller #5791/#6660 exist to bind: `sh -c "git worktree remove
/// 'unterminated"` from a subagent was allowed. Making this ONE function the
/// answer, called from `pm_guard`'s ABSOLUTE band ahead of every Bash rule,
/// is what stops a future rule from inheriting the same hole.
/// What: `Some(reason)` when any segment, at any wrapper depth, carries
/// `$'…'`/`$"…"` quoting the lexer mangles ([`shell_lex::has_live_ansi_c_quoting`]),
/// a wrapper whose inner command will not lex
/// ([`shell_lex::WrappedCommand::Unlexable`]), or a wrapper nested past
/// [`MAX_WRAPPER_DEPTH`]. `None` — the ordinary case — leaves every rule to
/// classify the command as before.
/// Test: `unclassifiable_command_flags_ansi_c_quoting`,
/// `unclassifiable_command_flags_an_unlexable_wrapper`,
/// `unclassifiable_command_denies_past_the_depth_cap`,
/// `unclassifiable_command_allows_ordinary_commands`, and end to end in
/// `tests/tm_hook_pm_guard.rs`.
pub(crate) fn unclassifiable_command(command: &str) -> Option<&'static str> {
    unclassifiable_at(command, 0)
}

/// Depth-aware core of [`unclassifiable_command`].
fn unclassifiable_at(command: &str, depth: usize) -> Option<&'static str> {
    for raw in split_shell_segments_raw(command) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if shell_lex::has_live_ansi_c_quoting(trimmed) {
            return Some(ANSI_C_QUOTING_REASON);
        }
        match shell_lex::wrapped_command(trimmed) {
            shell_lex::WrappedCommand::Unlexable => return Some(UNLEXABLE_WRAPPER_REASON),
            shell_lex::WrappedCommand::Inner(inner) => {
                // A wrapper AT the cap is refused, not skipped: the guard has
                // run out of budget to see what it hides.
                if depth >= MAX_WRAPPER_DEPTH {
                    return Some(WRAPPER_DEPTH_REASON);
                }
                if let Some(reason) = unclassifiable_at(&inner, depth + 1) {
                    return Some(reason);
                }
            }
            shell_lex::WrappedCommand::None => {}
        }
    }
    None
}

/// Maximum command-substitution recursion depth before conservatively denying.
///
/// Why: [`classify_command_substitutions`] recurses through
/// [`evaluate_bash_command`] on each `$(…)` / backtick body. Adversarial deep
/// nesting (`$($($(…`) would otherwise recurse without bound and could exhaust
/// the stack, crashing the short-lived `tm hook --pm-guard` process. Capping
/// the depth turns a crash into a (safe-direction) deny. The cap is generous —
/// real commands nest a handful of levels at most, never dozens.
/// What: the recursion budget threaded as `depth` through
/// [`evaluate_bash_command_inner`] → [`classify_bash_segment`] →
/// [`classify_command_substitutions`]. Past it, substitution scanning denies.
const MAX_SUBSTITUTION_DEPTH: usize = 32;

/// Classify a `Bash` command: `Some(reason)` denies, `None` allows.
///
/// Why: see the module docs — Bash composition and substitution are the seams
/// a prompt-only prohibition can't close, so every composed command (not just
/// the first token) must be inspected.
/// What: the depth-0 entry point; delegates to [`evaluate_bash_command_inner`]
/// which threads the substitution recursion budget.
/// Test: `evaluate_bash_command_denies_*`, `evaluate_bash_command_allows_*`,
/// `evaluate_bash_command_denies_composition_*`.
pub(crate) fn evaluate_bash_command(command: &str) -> Option<&'static str> {
    evaluate_bash_command_inner(command, 0)
}

/// Depth-aware core of [`evaluate_bash_command`].
///
/// Why: command substitutions recurse back into this classifier; carrying an
/// explicit `depth` lets [`classify_command_substitutions`] bound that
/// recursion (see [`MAX_SUBSTITUTION_DEPTH`]) instead of trusting the stack.
/// What: splits on the shell composition operators `&&`, `||`, `;`, `|`, a bare
/// `&`, and a newline (see [`split_shell_segments`]), runs
/// [`classify_bash_segment`] on each — denying if ANY segment names a forbidden
/// verb — then applies the whole-command file-write redirection check
/// ([`has_file_write_redirection`], which ignores `2>&1` fd-dups and
/// `/dev/null` discards). Benign pipes whose segments all allow still pass.
/// Empty commands allow. Deliberately quote-unaware: a forbidden verb hidden in
/// a quoted string may over-deny, the safe direction here.
/// Test: covered via `evaluate_bash_command_*` (this is its core).
fn evaluate_bash_command_inner(command: &str, depth: usize) -> Option<&'static str> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    // #6660: refuse before classifying — a command the guard cannot lex is a
    // command it cannot clear. The same check runs in `pm_guard`'s ABSOLUTE
    // band for the callers that never reach here.
    if let Some(reason) = unclassifiable_command(trimmed) {
        return Some(reason);
    }
    for segment in split_shell_segments(trimmed) {
        if let Some(reason) = classify_bash_segment(&segment, depth) {
            return Some(reason);
        }
    }
    if has_file_write_redirection(trimmed) {
        return Some(SHELL_EDIT_REASON);
    }
    None
}

/// Split a command into the sub-commands joined by a shell separator.
///
/// Why: a forbidden verb can hide in any composed segment, not just the first;
/// splitting lets [`evaluate_bash_command_inner`] classify each independently.
/// The separator set must be complete — a bare `&` (background/sequence
/// separator) and a newline are separators too, and omitting the `&` let
/// `true & sed -i …` slip through as one segment classified on its benign
/// leading verb (PR #1985).
/// What: a byte scan that cuts at each composition operator — the two-byte
/// `&&`/`||` (checked first so they are never seen as a bare `&`/`|`), a bare
/// `;`/`|`/`&`/newline — and returns the raw (untrimmed) segments;
/// `classify_bash_segment` trims. A bare `&` is NOT a split when it is a
/// redirection fd-dup (`>&`, `&>`, `2>&1`) or when nothing but whitespace
/// follows it (trailing background `foo &`), so those stay one segment.
/// Quote-aware (#2734): an operator that lies inside a quoted string is literal
/// data (`git commit -m 'a | b'`), not a separator, so it never splits — UNLESS
/// the command's quotes are unbalanced, in which case the [`QuoteScan`] map is
/// untrustworthy and we fall back to the original quote-unaware split (the
/// conservative, over-splitting direction that the sed/awk co-process detection
/// still relies on).
/// Test: `split_shell_segments_splits_operators`,
/// `split_shell_segments_splits_bare_ampersand`,
/// `split_shell_segments_single_command`,
/// `split_shell_segments_ignores_quoted_operators`.
///
/// #6660: the returned list also carries the commands hidden inside a leading
/// `sh -c` / `bash -c` / `env -S` / `xargs` wrapper — see
/// [`expand_shell_segments`]. Every rule in this module reads its segments from
/// here, which is what makes one descent reach all of them.
// #7266: `pub(crate)` so the read guard reads its segments from the SAME
// splitter every rule in this module already uses — that is what makes the
// `sh -c` descent above reach it too.
pub(crate) fn split_shell_segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    expand_shell_segments(command, 0, &mut out);
    out
}

/// Emit each of `command`'s segments, followed by the segments of whatever a
/// leading command wrapper would run (#6660).
///
/// Why: `sh -c "git worktree remove …"` reached every rule as a segment whose
/// program is `sh`, and no rule has an opinion about `sh`. Emitting the inner
/// command as ADDITIONAL segments is additive by construction: every segment
/// the caller saw before is still present, byte-identical and in the same
/// order, so no existing classification changes and the wrapped command is now
/// classified too.
///
/// The `cd`-tracking rules read these segments in order, so a `cd` inside a
/// wrapped string leaks into the segments that follow the wrapper — a subshell
/// `cd` does not really leak. That over-resolves a later relative path against
/// the subshell's directory, which is the over-blocking direction this guard
/// already prefers everywhere else.
/// What: recurses through [`shell_lex::wrapped_command`] up to
/// [`MAX_WRAPPER_DEPTH`] layers. An [`shell_lex::WrappedCommand::Unlexable`]
/// wrapper contributes no inner segments; [`classify_bash_segment`] denies it
/// instead.
/// Test: `wrappers_do_not_hide_the_inner_command_from_the_git_verb_rules`.
fn expand_shell_segments(command: &str, depth: usize, out: &mut Vec<String>) {
    for raw in split_shell_segments_raw(command) {
        out.push(raw.to_string());
        if depth >= MAX_WRAPPER_DEPTH {
            continue;
        }
        if let shell_lex::WrappedCommand::Inner(inner) = shell_lex::wrapped_command(raw.trim()) {
            expand_shell_segments(&inner, depth + 1, out);
        }
    }
}

/// The composition-operator split itself, without the #6660 wrapper descent.
///
/// Heredoc-aware (#6946): [`QuoteScan`] knows only `'` and `"`, so every bare
/// newline in a here-document body was a cut. `cat <<'EOF' > notes.md` /
/// `git commit -m wip` / `EOF` arrived at the ADR-0049 composed-commit guard as
/// three segments, and the guard denied a call that runs no git at all.
/// [`heredoc::HeredocBodies::suppresses_separator`] marks the body, the newline
/// that opened it, and the terminator line as data, so the whole here-document
/// stays one segment. The operator line keeps its live syntax, an unterminated
/// here-document suppresses nothing, and a body handed to a shell still splits.
/// Test: `split_shell_segments_keeps_a_heredoc_body_whole`,
/// `split_shell_segments_still_splits_an_unterminated_heredoc`,
/// `split_shell_segments_still_splits_a_shell_heredoc_body`.
fn split_shell_segments_raw(command: &str) -> Vec<&str> {
    let scan = QuoteScan::new(command);
    let quoted = |i: usize| scan.balanced && !scan.is_unquoted(i);
    let bodies = heredoc::HeredocBodies::scan(command);
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        // #6946: a here-document body and its terminator are data, not syntax.
        if quoted(i) || bodies.suppresses_separator(i) {
            i += 1;
            continue;
        }
        let two = command.get(i..i + 2);
        if two == Some("&&") || two == Some("||") {
            segments.push(&command[start..i]);
            i += 2;
            start = i;
            continue;
        }
        if bytes[i] == b';' || bytes[i] == b'|' || bytes[i] == b'\n' {
            segments.push(&command[start..i]);
            i += 1;
            start = i;
            continue;
        }
        if bytes[i] == b'&' {
            // A bare `&` is a separator EXCEPT when it is a redirection fd-dup
            // (`>&fd` / `2>&1` — preceded by `>`; `&>file` — followed by `>`)
            // or trailing background with nothing after it (`foo &`).
            let prev_is_redirect = i > 0 && bytes[i - 1] == b'>';
            let next_is_redirect = i + 1 < bytes.len() && bytes[i + 1] == b'>';
            let nothing_follows = command[i + 1..].trim().is_empty();
            if prev_is_redirect || next_is_redirect || nothing_follows {
                i += 1;
                continue;
            }
            segments.push(&command[start..i]);
            i += 1;
            start = i;
            continue;
        }
        i += 1;
    }
    segments.push(&command[start..]);
    segments
}

/// Classify a single composition segment: `Some(reason)` denies, `None` allows.
///
/// Why: factored out of [`evaluate_bash_command_inner`] so the same first-token
/// / two-token / substitution deny logic runs uniformly on every segment.
/// What: resolves the effective program via [`first_command_token`] (strips
/// env/`sudo`/path noise) and denies `patch` (shell edit, unconditionally — it
/// has no read-only use case); `sed`/`awk`/`gawk`/`nawk`/`mawk` are
/// deny-by-default and allowed only when [`sed_awk::sed_is_readonly`] /
/// [`sed_awk::awk_is_readonly`] proves the segment narrowly read-only (issue
/// #2664 — the earlier allow-by-default-unless-flagged shape missed
/// `awk 'BEGIN{system(...)}'`, sed `e`/`w`/`W` commands, and `-f`/`--file`
/// external scripts); `curl`/`wget` (network); `make`/`pytest` (build/test);
/// matches the two-token forms `git apply` / `npm test` via
/// [`effective_tool_name`]; and finally inspects any command substitution /
/// subshell in the segment via [`classify_command_substitutions`] (carrying
/// the recursion `depth`). Empty/whitespace segments allow.
/// Test: covered via `evaluate_bash_command_*` (this is its per-segment core).
fn classify_bash_segment(segment: &str, depth: usize) -> Option<&'static str> {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(program) = first_command_token(trimmed) {
        match program.as_str() {
            "patch" => return Some(SHELL_EDIT_REASON),
            "sed" if !sed_awk::sed_is_readonly(trimmed) => return Some(SHELL_EDIT_REASON),
            "awk" | "gawk" | "nawk" | "mawk" if !sed_awk::awk_is_readonly(trimmed) => {
                return Some(SHELL_EDIT_REASON);
            }
            "curl" | "wget" => return Some(NETWORK_REASON),
            "make" | "pytest" => return Some(BUILD_TEST_REASON),
            // `git apply` edits files. Resolve the real subcommand through any
            // leading git global flags (`-C <path>`, `-c <kv>`, `--git-dir=…`)
            // via [`shell_lex::git_subcommand`] (#2734) so `git -C <path> apply`
            // is still caught and `git -C <path> commit` is NOT mis-denied — the
            // two-token `effective_tool_name` matcher below cannot see past the
            // global flags. On unbalanced quotes `git_subcommand` yields `None`
            // and we simply don't treat it as `git apply` (matching the prior
            // allow-on-ambiguous-git-command behaviour).
            "git" if shell_lex::git_subcommand(trimmed).as_deref() == Some("apply") => {
                return Some(SHELL_EDIT_REASON);
            }
            // #7399: `git diff --output=<file>`, `git format-patch -o <dir>`,
            // `git archive -o <file>` and `git bundle create <file>` all write
            // with no `>`, so the redirection check below never saw them.
            "git" if shell_lex::git_file_write_target(trimmed).is_some() => {
                return Some(SHELL_EDIT_REASON);
            }
            _ => {}
        }
    }
    if effective_tool_name(trimmed) == "npm test" {
        return Some(BUILD_TEST_REASON);
    }
    classify_command_substitutions(trimmed, depth)
}

/// Whether a paren-delimited substitution opens at byte `i`, and whether it is
/// live there given the segment's quote map.
///
/// Why (#2745): `$(…)`, `<(…)` and `>(…)` are one CLASS — bash executes the
/// body of each, so each must be decomposed and classified. Naming them in one
/// place is what makes that true by construction: a spelling added here is
/// scanned by [`classify_command_substitutions`] with no other edit, and the
/// gap that let `diff <(sed -i …) x` through cannot reopen one form at a time.
/// The two forms differ in ONE respect, which is why this returns liveness
/// rather than a bare bool: double quotes suppress process substitution
/// (`echo "<(x)"` is literal text) but NOT command substitution
/// (`echo "$(sed -i …)"` still runs `sed`).
/// What: `Some(true)` when a substitution opens at `i` and is live shell
/// syntax there, `Some(false)` when one opens but is quoted into literal text,
/// `None` when no substitution opens at `i`. On unbalanced quotes the map is
/// untrustworthy, so every opener reads as live — the conservative direction.
/// Test: `evaluate_bash_command_denies_process_substitution_edit`,
/// `evaluate_bash_command_allows_quoted_process_substitution_prose`,
/// `evaluate_bash_command_allows_quoted_substitution_prose`.
fn paren_substitution_live_at(scan: &QuoteScan, bytes: &[u8], i: usize) -> Option<bool> {
    if bytes.get(i + 1).copied() != Some(b'(') {
        return None;
    }
    match bytes[i] {
        b'$' => Some(!scan.balanced || scan.allows_substitution(i)),
        b'<' | b'>' => Some(!scan.balanced || scan.is_unquoted(i)),
        _ => None,
    }
}

/// Inspect every substitution in a segment — `$(…)`, `<(…)`, `>(…)`, and
/// backticks — for hidden forbidden verbs.
///
/// Why: first-token classification sees the *outer* command only, so
/// `echo "$(sed -i s/a/b/ f)"` would pass on `echo` while the substitution
/// silently runs `sed`. Since the dangerous direction here is a MISSED deny,
/// this scans substitutions too. Design choice: rather than blanket-deny every
/// substitution (which would break trivial, ubiquitous forms like
/// `echo "$(date)"`), it recursively classifies the *body* of each with
/// [`evaluate_bash_command_inner`] — benign body allows, forbidden one denies.
/// Unbalanced substitutions (an opening `$(`/`<(`/`>(`/backtick with no
/// matching close) deny conservatively, and so does over-deep nesting: an
/// adversarial `$($($(…` chain is bounded by [`MAX_SUBSTITUTION_DEPTH`] so it
/// can never exhaust the stack and crash the guard process (a deny is the safe
/// outcome).
///
/// #2745: process substitution used to be invisible here, so
/// `diff <(sed -i s/a/b/ f) x` was ALLOWED while bash ran the `sed -i`, and
/// `>(…)` denied only incidentally — its leading `>` tripped
/// [`has_file_write_redirection`], never its body. Both now classify like every
/// other substitution, via [`paren_substitution_live_at`].
/// What: past the depth cap, returns [`SHELL_EDIT_REASON`] immediately.
/// Otherwise byte-scans for each paren-delimited opener (matching `)` with
/// paren-depth tracking) and for backtick pairs, recursively evaluates each
/// balanced body at `depth + 1`, and propagates a deny; an unbalanced
/// substitution returns [`SHELL_EDIT_REASON`].
/// Test: `evaluate_bash_command_denies_hidden_substitution_verb`,
/// `evaluate_bash_command_allows_benign_substitution`,
/// `evaluate_bash_command_denies_unbalanced_substitution`,
/// `evaluate_bash_command_bounds_deep_substitution_nesting`,
/// `evaluate_bash_command_allows_quoted_substitution_prose`,
/// `evaluate_bash_command_denies_process_substitution_edit`,
/// `evaluate_bash_command_denies_output_process_substitution_by_classification`,
/// `evaluate_bash_command_allows_readonly_process_substitution`,
/// `evaluate_bash_command_denies_unbalanced_process_substitution`.
fn classify_command_substitutions(segment: &str, depth: usize) -> Option<&'static str> {
    if depth >= MAX_SUBSTITUTION_DEPTH {
        // Too deeply nested to safely decompose — deny conservatively rather
        // than recurse further and risk a stack overflow.
        return Some(SHELL_EDIT_REASON);
    }
    let scan = QuoteScan::new(segment);
    let backtick_live = |i: usize| !scan.balanced || scan.allows_substitution(i);
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(live) = paren_substitution_live_at(&scan, bytes, i) {
            if !live {
                i += 1;
                continue;
            }
            let mut paren = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && paren > 0 {
                match bytes[j] {
                    b'(' => paren += 1,
                    b')' => paren -= 1,
                    _ => {}
                }
                j += 1;
            }
            if paren != 0 {
                // Unbalanced opener — cannot decompose; deny conservatively.
                return Some(SHELL_EDIT_REASON);
            }
            if let Some(reason) = evaluate_bash_command_inner(&segment[i + 2..j - 1], depth + 1) {
                return Some(reason);
            }
            i = j;
            continue;
        }
        if bytes[i] == b'`' && backtick_live(i) {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'`' {
                j += 1;
            }
            if j >= bytes.len() {
                // Unbalanced backtick — cannot decompose; deny conservatively.
                return Some(SHELL_EDIT_REASON);
            }
            if let Some(reason) = evaluate_bash_command_inner(&segment[i + 1..j], depth + 1) {
                return Some(reason);
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    None
}

/// Best-effort target path for a shell command already classified as a
/// [`SHELL_EDIT_REASON`] deny, for content-aware delegation routing (issue
/// #2918).
///
/// Why: `pm_guard`'s denial message used to hardcode "delegate to
/// rust-engineer" no matter what file the shell command actually touched.
/// Routing by content type needs the target path; unlike the Edit/Write tools
/// (which name it directly in `tool_input.file_path`), a Bash command only has
/// its target embedded in the command text itself.
/// What: scans each composition segment ([`split_shell_segments`]) for a real
/// file-write redirect ([`redirection_target`]), for the file a git write
/// option names ([`shell_lex::git_file_write_target`], #7399), or, for a
/// sed/awk-family/`patch`/`git apply` segment, the command's trailing
/// non-flag token ([`trailing_file_token`]) — the conventional position of the
/// target file for those verbs. Returns the first match found; `None` when no
/// segment yields a plausible target (the caller then falls back to the
/// generic delegation hint). The sed/awk-family half is a best-effort HINT
/// only — its trailing token may name a file the command READS. The positively
/// identified half is [`shell_write_target`], which the write boundary decides
/// on.
/// Test: `extract_shell_edit_target_*`.
pub(crate) fn extract_shell_edit_target(command: &str) -> Option<String> {
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(target) = segment_write_target(trimmed) {
            return Some(target);
        }
        if let Some(program) = first_command_token(trimmed) {
            let program = program.as_str();
            let is_sed_awk_family =
                matches!(program, "patch" | "sed" | "awk" | "gawk" | "nawk" | "mawk");
            let is_git_apply =
                program == "git" && shell_lex::git_subcommand(trimmed).as_deref() == Some("apply");
            if (is_sed_awk_family || is_git_apply)
                && let Some(target) = trailing_file_token(trimmed)
            {
                return Some(target);
            }
        }
    }
    None
}

/// The file a Bash command would WRITE, when one is positively identified.
///
/// Why (#7399): the main-checkout write boundary (ADR-0044, enforced by
/// ADR-0048) asks WHERE a write lands, and until now it could only ask that of
/// the Edit/Write tools — a Bash write reached `SHELL_EDIT_REASON`, which asks
/// WHO is writing and is budget-tiered, so `git diff --output=<file>` and
/// `echo … > <file>` both landed a source file in a shared main checkout
/// within budget. This is the half of [`extract_shell_edit_target`] the
/// boundary can decide on: a redirect and a git write option each NAME the file
/// git or the shell will create, with no reading arm. The sed/awk trailing
/// token is deliberately excluded — `sed -n '1,5p' <file>` puts a file it only
/// READS in that same position, so deciding a deny on it would refuse reads.
/// What: the first [`segment_write_target`] across the command's composition
/// segments ([`split_shell_segments`]). `None` when no segment names a write —
/// which includes every command the guard cannot lex, so this can never turn an
/// existing allow into a deny on a parse failure.
/// Test: `shell_write_target_reads_redirects_and_git_output`,
/// `shell_write_target_ignores_reads`, and end to end in
/// `pm_guard_denies_a_git_output_write_in_a_main_checkout`.
pub(crate) fn shell_write_target(command: &str) -> Option<String> {
    split_shell_segments(command)
        .into_iter()
        .find_map(|segment| segment_write_target(segment.trim()))
}

/// The file ONE command segment would write, if its text names one.
///
/// Why: [`extract_shell_edit_target`] and [`shell_write_target`] ask the same
/// question of a segment and must never drift apart — one rule about what a
/// segment writes, read by the routing hint and by the write boundary alike.
/// What: the redirect target ([`redirection_target`]) or, on a `git` segment,
/// the file a git write option names ([`shell_lex::git_file_write_target`]).
/// A git write that names no readable path (a valueless `--output`, a bare
/// `format-patch`) yields an empty string from that function and is skipped
/// here: the write is real and `classify_bash_segment` still denies it, but
/// there is no path for the boundary to place.
/// Test: `shell_write_target_reads_redirects_and_git_output`.
fn segment_write_target(segment: &str) -> Option<String> {
    if segment.is_empty() {
        return None;
    }
    if let Some(target) = redirection_target(segment) {
        return Some(target);
    }
    // #7399: a git write option names its file in the option, not in the
    // trailing position the sed/awk verbs use.
    if first_command_token(segment).as_deref() == Some("git")
        && let Some(target) = shell_lex::git_file_write_target(segment)
        && !target.is_empty()
    {
        return Some(target);
    }
    None
}

/// The file a real write redirect in `command` names, if any.
///
/// Why (#7399 review, HIGH): this scan used to exist twice — once returning a
/// bool for [`has_file_write_redirection`] and once returning the token for
/// [`redirection_target`] — and only the bool copy learned #5356's heredoc
/// skip. Once the write boundary began deciding a hard, budget-exempt deny on
/// the token copy, that drift meant a `>` in here-document PROSE denied a
/// command that writes nothing. One scanner, two thin callers, so the two
/// cannot re-diverge.
/// What: scans for `>` outside quotes ([`QuoteScan`]) and outside
/// here-document bodies ([`heredoc::HeredocBodies`]); skips a second `>`
/// (append) and any spaces, treats a following `&` as an fd-duplication
/// (`2>&1`, `>&2`) and `/dev/null` as an output discard, and returns the
/// target token of the first real file-write redirect. `Some("")` when the
/// redirect is real but names no token (`cmd >|`, a trailing `>`): still a
/// write for the bool caller, no path for the boundary.
/// Test: `has_file_write_redirection_*`,
/// `extract_shell_edit_target_from_redirection`,
/// `shell_write_target_ignores_a_heredoc_body_redirect`.
fn scan_file_write_redirect(command: &str) -> Option<String> {
    let scan = QuoteScan::new(command);
    let bodies = heredoc::HeredocBodies::scan(command);
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if scan.balanced && !scan.is_unquoted(i) {
            i += 1;
            continue;
        }
        // #5356: a here-document body is data, not shell syntax.
        if bodies.contains(i) {
            i += 1;
            continue;
        }
        if bytes[i] == b'>' {
            let mut j = i + 1;
            // `>>` append is still a file write; skip the second `>`.
            if j < bytes.len() && bytes[j] == b'>' {
                j += 1;
            }
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            // `>&fd` / `2>&1` duplicate a descriptor — not a file write.
            if j < bytes.len() && bytes[j] == b'&' {
                i = j + 1;
                continue;
            }
            // `/dev/null` is an output-discard sink, not a file write
            // (`which cargo 2>/dev/null`) — allow it and keep scanning.
            // #7399 review: a newline and a tab end a shell word exactly as a
            // space does. Without them `python3 <<'PY' > out.rs\nprint(1)\nPY`
            // read its target as `out.rs\nprint(1)\nPY`, and `cmd >/dev/null`
            // followed by a newline read as `/dev/null\n…`, missing the
            // discard and denying a benign command.
            let start = j;
            while j < bytes.len()
                && !matches!(
                    bytes[j],
                    b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'<' | b'|' | b';' | b'&'
                )
            {
                j += 1;
            }
            let target = &command[start..j];
            if target == "/dev/null" {
                i = j;
                continue;
            }
            return Some(target.to_string());
        }
        i += 1;
    }
    None
}

/// The real file-write redirect target in `command`, if any (owned-string
/// sibling of [`has_file_write_redirection`], for routing-hint extraction and
/// for the write boundary rather than a pure yes/no classification).
///
/// What: [`scan_file_write_redirect`], with the no-token case dropped — a
/// redirect that names nothing gives a caller asking "which file" no answer.
/// Test: `extract_shell_edit_target_from_redirection`.
fn redirection_target(command: &str) -> Option<String> {
    scan_file_write_redirect(command).filter(|target| !target.is_empty())
}

/// The trailing non-flag whitespace-separated token of `command`.
///
/// Why: `sed -i s/a/b/ file.rs`, `patch -p1 file.diff`, and `git apply
/// my.patch` all conventionally place the target file last; this is the cheap
/// heuristic [`extract_shell_edit_target`] uses for those verbs.
/// What: the last `split_whitespace` token, unless it starts with `-` (an
/// option flag with nothing after it) in which case there is no plausible
/// target.
fn trailing_file_token(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .next_back()
        .filter(|t| !t.starts_with('-'))
        .map(str::to_string)
}

/// Whether `command` redirects output to a *file* (a filesystem write), as
/// opposed to an fd-duplication like `2>&1` / `>&2`.
///
/// Why: `echo 'code' > src/lib.rs` writes a file just as surely as the `Write`
/// tool does — the enforcement would be trivially bypassable without catching
/// redirection. But blanket-denying every `>` would false-positive on the very
/// common `… 2>&1` / `>&2` fd redirects, which are not file writes, so those
/// must be distinguished.
/// What: `true` when [`scan_file_write_redirect`] — the one scanner this and
/// [`redirection_target`] share since #7399 — finds a redirect. It skips a
/// second `>` (append) and any spaces, treats it as an fd-duplication (allow,
/// keep scanning) only when the next non-space byte is `&`, then reads the
/// redirect *target* token and treats `/dev/null` as benign (output discard,
/// e.g. `2>/dev/null` / `>/dev/null` / `&>/dev/null`) — allow, keep scanning.
/// Any other `>` is a file-write redirect → `true`.
/// Quote-aware (#2734): a `>` inside a quoted string is literal argument content
/// (`git commit -m 'spec -> code'` — Bob's live false positive), not a
/// redirection, and is skipped — UNLESS the command's quotes are unbalanced, in
/// which case the [`QuoteScan`] map is untrustworthy and every `>` is scanned
/// (the original conservative, over-blocking behaviour). Shell-level redirects
/// (`echo x > f`) are unquoted and still caught. NOTE: an in-quote `>` that is
/// genuinely dangerous — an `awk 'BEGIN{print > "f"}'` in-program file write —
/// is caught by [`sed_awk::awk_is_readonly`], not here.
/// Heredoc-aware (#5356): [`QuoteScan`] knows only `'` and `"`, so a `>` in a
/// here-document body — a `len(k) > 3` comparison in a `python3 <<'PY'`
/// script, an `->` arrow in `cat <<EOF` prose — read as a redirect and made a
/// pure read consume the PM's file-change budget. [`heredoc::HeredocBodies`]
/// marks those bytes as body content; the operator line itself stays live, so
/// `python3 <<'PY' > out.rs` is still a redirect.
/// Test: `has_file_write_redirection_detects_write`,
/// `has_file_write_redirection_detects_append`,
/// `has_file_write_redirection_ignores_fd_dup`,
/// `has_file_write_redirection_ignores_dev_null`,
/// `has_file_write_redirection_ignores_quoted_gt`,
/// `has_file_write_redirection_ignores_heredoc_body`,
/// `has_file_write_redirection_detects_redirect_on_a_heredoc_operator_line`,
/// `has_file_write_redirection_false_for_plain_command`.
pub(crate) fn has_file_write_redirection(command: &str) -> bool {
    scan_file_write_redirect(command).is_some()
}

/// Deny reason for `git worktree add` targeting a denylisted temp root.
///
/// Why (issue #3977, filed as the worktree-hygiene follow-up to #3955): a
/// worktree provisioned under `/tmp`/`/private/tmp`/`/var/folders` silently
/// fails unrelated tests (trusty-search's `SENSITIVE_PATH_PREFIXES` denylist,
/// #3955) and has repeatedly caused agents to lose in-flight work when the
/// harness or OS reaps that scratch space out from under a live worktree.
/// The message must be actionable, not a bare refusal, so it names the
/// project-local convention directly.
pub(crate) const WORKTREE_TMP_REASON: &str = "`git worktree add` must not target /tmp, \
     /private/tmp, /var/folders, $TMPDIR, or the harness scratchpad — worktrees provisioned \
     there silently fail unrelated tests and can be reaped mid-task (issue #3955). Use \
     `<repo-root>/.claude/worktrees/<name>` (the tm-workflow convention) instead.";

/// Filesystem roots `git worktree add` must never target (issue #3977).
///
/// Why: `/tmp` and `/private/tmp` are the same location on macOS (`/tmp` is a
/// symlink) but both are listed literally because [`resolves_under_denylisted_tmp`]
/// does a lexical prefix match, not a filesystem-resolving one (see that
/// function's doc for why). `/var/folders` is the root `$TMPDIR` resolves
/// under on macOS (e.g. `/var/folders/x1/…/T/`), and `/var` is itself a
/// symlink to `/private/var`, so `/private/var/folders` is the SAME location
/// under its resolved spelling and needs the same paired entry `/tmp` already
/// has. `current_dir()` inside `$TMPDIR` returns that resolved spelling, so
/// without this entry a cwd-relative target resolved from a
/// `/private/var/folders` working directory slipped past the guard (#5924).
/// The harness scratchpad (`/private/tmp/claude-502/…`) is already covered by
/// the `/private/tmp` entry — it is called out separately in the deny message
/// (not here) because agents are told to prefer it "instead of /tmp", making
/// it the subtle case that looks compliant while still landing in a
/// denylisted zone.
const WORKTREE_TMP_DENYLIST_ROOTS: &[&str] = &[
    "/tmp",
    "/private/tmp",
    "/var/folders",
    // #5924: /var is a symlink to /private/var, so $TMPDIR's resolved spelling
    // needs the same paired entry /tmp already has.
    "/private/var/folders",
];

/// `git worktree add`'s own flags that consume a following argv token.
///
/// Why: needed so [`worktree_add_target_token`] can skip past `-b <branch>` /
/// `-B <branch>` / `--reason <text>` (used with `--lock`) without mistaking
/// the flag's value for the target path.
const WORKTREE_ADD_FLAGS_WITH_ARG: &[&str] = &["-b", "-B", "--reason"];

/// Detect a `git worktree add` call whose target resolves under a denylisted
/// temp root, regardless of who dispatches the `Bash` call.
///
/// Why: this is the ONE piece of policy that must be called directly from
/// `pm_guard()` **before** its Guard 4 subagent-exemption early return, NOT
/// routed through [`evaluate_tool`](crate::commands::pm_guard::evaluate_tool)/[`evaluate_bash_command`] like every other
/// Bash rule in this module. `evaluate_bash_command` is the sole callee of
/// `evaluate_tool`, which Guard 4 never reaches for a payload carrying a
/// non-empty `agent_id` — i.e. every native Task/Agent-dispatched subagent,
/// which is exactly who provisions these worktrees in practice. A rule placed
/// inside `evaluate_bash_command`/`classify_bash_segment` would therefore be a
/// silent no-op for that calling pattern. See `pm_guard::pm_guard`'s call site
/// for the enforcement of "before Guard 4", and its comment for why that must
/// never be "simplified" back inside `evaluate_tool`.
/// What: splits `command` into composition segments (reusing
/// [`split_shell_segments`] for consistency with the rest of the module),
/// tracks a running effective working directory across the command line by
/// following `cd <dir>` segments and a leading `git -C <dir>` override (a
/// deliberate, PARTIAL closing of the `cd /tmp && git worktree add wt-foo`
/// bypass — see the "Residual bypasses" note below), resolves each `worktree
/// add` segment's target (skipping `-b`/`-B`/`--reason` flag values) against
/// that directory, lexically normalizes it (collapsing `.`/`..` WITHOUT
/// touching the filesystem — the target usually does not exist yet), expands
/// a leading `~` and any `$TMPDIR`/`${TMPDIR}`/`$TMP`/`${TMP}` occurrence
/// using the guard process's own environment (which a Bash tool call inherits
/// unchanged), and denies when the result lexically starts with one of
/// [`WORKTREE_TMP_DENYLIST_ROOTS`]. Only the exact `worktree add` two-token
/// form denies — `list`/`remove`/`prune`/`lock`/… are left alone, matching
/// git's own subcommand structure (`git worktree <subcmd>`, not
/// `git worktree add <subcmd>`), and no other `git`/shell verb is touched.
///
/// Residual bypasses accepted (documented per the task's explicit instruction
/// to state, not hide, what remains open — a missed instance here fails open
/// to ALLOW, same as every other classifier in this module):
/// - Indirection through an intermediate shell variable
///   (`X=/tmp; git worktree add "$X/foo"`) is not resolved — this module does
///   not implement a shell variable table.
/// - A `cd`/`-C` target that is itself a symlink into a denylisted root
///   (e.g. a project-tree symlink pointing at `/tmp`) is not caught: path
///   resolution here is purely lexical, never touches the filesystem
///   (`fs::canonicalize`), because the worktree target usually does not exist
///   yet and a guard must stay fast and side-effect-free.
///   `resolves_under_denylisted_tmp` therefore cannot see through a symlink
///   the way the OS eventually would.
///   `/tmp` itself being a symlink to `/private/tmp`, and `/var` to
///   `/private/var`, are NOT in this bucket — both spellings of each are
///   literal entries in [`WORKTREE_TMP_DENYLIST_ROOTS`] (#5924), so a literal
///   `/tmp/...` or `/private/var/folders/...` argument is caught without
///   needing to resolve the symlink.
/// - Multiple/cumulative `git -C` flags are collapsed to "the last one
///   present"; real git applies them successively relative to each other.
///   Realistic invocations use at most one `-C`.
/// - A `cd`/`-C` argument built from command substitution
///   (`cd "$(mktemp -d)"`) is not resolved — this module classifies text
///   structurally, it does not execute the shell.
/// - **Largest of this list, tracked separately as
///   [issue #3981](https://github.com/bobmatnyc/trusty-tools/issues/3981):**
///   `pm_guard`'s Guard 2/3 escape hatches
///   (`TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED`) bypass this
///   entire guard when set, and while neither is ever set programmatically
///   in this codebase's Rust source, Claude Code's `settings.json` supports a
///   top-level `env` object applied to every hook invocation (live-reloaded
///   mid-session) — and a `Write`/`Edit` to `.claude/settings.json` is not
///   itself blocked by `pm_guard` today (`.json` is not a source-code
///   extension, so `evaluate_edit_tool` falls through to an unconditional
///   ALLOW). So the PM, or any subagent Guard 4 already exempts, can set
///   either var there and self-exempt from every rule in this module,
///   including this one, in one ordinary already-permitted config write. This
///   is a pre-existing gap (Guard 3 predates this worktree guard by many
///   issues) that this change neither introduces nor widens; it guards
///   against an agent going off-script, not a determined adversary, and is
///   deliberately NOT fixed here — the remedy belongs in the edit-tool
///   classifier, not this Bash classifier, and is the project owner's call.
///
/// Test: `evaluate_worktree_add_command_*` below;
/// `pm_guard_blocks_worktree_add_under_tmp_via_subagent_payload` and siblings
/// in `tests/tm_hook_pm_guard.rs` exercise the end-to-end binary path,
/// including the `agent_id`-present case this function exists for.
pub(crate) fn evaluate_worktree_add_command(command: &str, cwd: &Path) -> Option<&'static str> {
    evaluate_worktree_add_command_in(command, cwd, &PathEnv::from_process())
}

/// Both `git worktree add` gates in one call: the temp-root denylist (#3955)
/// and the disk-usage threshold (#7497).
///
/// Why: `pm_guard()` must run both BEFORE its Guard 4 subagent exemption, and
/// two call sites there are two places for the next rule to be added to only
/// one of them. The placement requirement is identical, so the call is too.
/// What: the temp-root reason wins when both apply — it names a location that
/// is wrong regardless of how full the disk is. Returns `None` when the command
/// creates no worktree.
/// Test: `evaluate_worktree_add_command_*` for the first rule, `disk_usage`'s
/// own tests for the second, and `tests/tm_hook_pm_guard.rs` for both end to end.
pub(crate) fn evaluate_worktree_add(command: &str, cwd: &Path) -> Option<String> {
    evaluate_worktree_add_command(command, cwd)
        .map(str::to_string)
        .or_else(|| disk_usage::evaluate_worktree_add_disk_usage(command, cwd))
}

/// [`evaluate_worktree_add_command`] against an explicit environment.
///
/// Why: see [`PathEnv`] — this is the seam that lets the expansion rules be
/// asserted end-to-end (same segment splitting, same denylist, same return
/// value) without any test touching process-global env.
fn evaluate_worktree_add_command_in(
    command: &str,
    cwd: &Path,
    env: &PathEnv,
) -> Option<&'static str> {
    worktree_add_targets_in(command, cwd, env)
        .iter()
        .any(|target| resolves_under_denylisted_tmp(target))
        .then_some(WORKTREE_TMP_REASON)
}

/// Every path a `git worktree add` in `command` would create.
///
/// Why (#7497): the disk-usage gate asks a DIFFERENT question of the SAME
/// targets — "is the volume this lands on full?" rather than "is it under a
/// denylisted temp root?" — and a second walk of the command line is how the
/// two would drift into resolving `cd`, `git -C`, `~` and `$TMPDIR` differently.
/// One resolver, two rules.
/// What: the resolved, lexically normalised targets, in command order. Empty
/// when the command contains no `worktree add`, which is the fast path every
/// other Bash call takes.
/// Test: `worktree_add_targets_resolves_cd_and_dash_c`, and the disk rule's own
/// cases in `pm_guard_bash::disk_usage`.
pub(crate) fn worktree_add_targets(command: &str, cwd: &Path) -> Vec<PathBuf> {
    worktree_add_targets_in(command, cwd, &PathEnv::from_process())
}

/// [`worktree_add_targets`] against an explicit environment — see [`PathEnv`].
fn worktree_add_targets_in(command: &str, cwd: &Path, env: &PathEnv) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let mut effective_cwd = cwd.to_path_buf();
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_command_token(trimmed).as_deref() == Some("cd") {
            if let Some(argv) = shlex::split(trimmed)
                && let Some(dest) = argv.get(1)
            {
                effective_cwd = resolve_target_path(dest, &effective_cwd, env);
            }
            continue;
        }
        if shell_lex::git_subcommand(trimmed).as_deref() != Some("worktree") {
            continue;
        }
        let Some(argv) = shlex::split(trimmed) else {
            continue;
        };
        let Some(worktree_idx) = argv
            .windows(2)
            .position(|w| w[0] == "worktree" && w[1] == "add")
        else {
            // Not the `add` subcommand (list/remove/prune/lock/…) — never
            // blocked.
            continue;
        };
        let base = match git_dash_c_override(&argv, worktree_idx) {
            Some(dash_c) => resolve_target_path(dash_c, &effective_cwd, env),
            None => effective_cwd.clone(),
        };
        let Some(target) = worktree_add_target_token(&argv[worktree_idx + 2..]) else {
            continue;
        };
        targets.push(resolve_target_path(&target, &base, env));
    }
    targets
}

/// The `-C <path>` value preceding a resolved `worktree` subcommand token, if
/// any — git's own global working-directory override.
///
/// Why: `git -C /tmp worktree add wt-foo` changes git's effective working
/// directory for the whole invocation exactly as a preceding `cd /tmp &&`
/// would, so it must feed the same resolution base as
/// [`evaluate_worktree_add_command`]'s `cd`-tracking. Scoped to `argv[..worktree_idx]`
/// so a `-C` appearing after `worktree` (not valid git syntax, but shlex would
/// still tokenize it) is never mistaken for the global option.
/// What: the last `-C <value>` pair found before `worktree_idx` (real git
/// applies multiple `-C` flags cumulatively/relative to each other — this
/// collapses to "last wins", a documented simplification; see the residual
/// bypass list on [`evaluate_worktree_add_command`]).
fn git_dash_c_override(argv: &[String], worktree_idx: usize) -> Option<&str> {
    let mut i = 0;
    let mut last = None;
    while i < worktree_idx {
        if argv[i] == "-C" && i + 1 < worktree_idx {
            last = Some(argv[i + 1].as_str());
            i += 2;
            continue;
        }
        i += 1;
    }
    last
}

/// The target-path token of a `git worktree add …` argv tail (everything
/// after the `add` token), skipping flags and their values.
///
/// Why: `git worktree add [-f] [--detach] [--checkout] [--lock [--reason
/// <text>]] [--orphan] [(-b | -B) <branch>] <path> [<commit-ish>]` — the path
/// is the first non-flag positional, but `-b`/`-B`/`--reason` each consume a
/// following token that must not be mistaken for it.
/// What: walks `tail`, skipping [`WORKTREE_ADD_FLAGS_WITH_ARG`] pairs and any
/// other `-`-prefixed flag, returning the first remaining token.
fn worktree_add_target_token(tail: &[String]) -> Option<String> {
    let mut i = 0;
    while i < tail.len() {
        let tok = &tail[i];
        if WORKTREE_ADD_FLAGS_WITH_ARG.contains(&tok.as_str()) {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(tok.clone());
    }
    None
}

/// Whether `path` lexically starts with one of [`WORKTREE_TMP_DENYLIST_ROOTS`].
fn resolves_under_denylisted_tmp(path: &Path) -> bool {
    WORKTREE_TMP_DENYLIST_ROOTS
        .iter()
        .any(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests;
