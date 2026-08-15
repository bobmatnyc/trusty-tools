//! Bash-command classification for the `tm hook --pm-guard` PreToolUse guard
//! (issue #1977, hardened in PR #1985; sed/awk deny-by-default redesign #2664).
//!
//! Why: Bash is the escape hatch a prompt-only prohibition can't close — a PM
//! can edit a file with `sed -i`, write one with `echo … > f.rs`, apply a diff
//! with `git apply`, or run the suite with `pytest`, all of which side-step the
//! P1–P5 prohibitions. Worse, shell *composition* (`&&`, `||`, `;`, `|`, a bare
//! `&`, a newline) and command *substitution* (`$(…)`, backticks) are
//! themselves bypasses: a benign leading verb can hide a forbidden one further
//! down the line or inside a substitution. This module is the quote-unaware,
//! conservatively-over-blocking classifier that closes those seams; it is
//! factored out of `pm_guard.rs` so that file stays under the 500-SLOC cap.
//! What: [`evaluate_bash_command`] splits a command on every composition
//! separator, classifies each segment (first-token verb, two-token
//! `git apply` / `npm test`, and recursively any command substitution), and
//! applies a whole-command file-write redirection check. A missed deny is the
//! dangerous direction, so ambiguous forms (unbalanced substitutions,
//! over-deep nesting) deny. `sed`/`awk`-family verbs are classified by the
//! sibling [`sed_awk`] module, which is deny-by-default: a segment must prove
//! it is narrowly read-only (no in-place flag, no external script load, no
//! write/exec script construct, balanced quotes) to be allowed. The sibling
//! [`persistence`] module runs the one ALLOW-list here — the agent-cost stop's
//! escape hatch (#4837) — and is default-deny in the opposite direction. The
//! sibling [`main_checkout`] module carries the second rule `pm_guard` calls
//! directly, ahead of the subagent exemptions: whole-tree-destructive git
//! verbs aimed at a project's main checkout (ADR-0037).
//! Test: `evaluate_bash_command_*`, `split_shell_segments_*`, and
//! `has_file_write_redirection_*` in this module's `tests` submodule;
//! `sed_awk::tests` for the sed/awk-specific safety analysis.

mod main_checkout;
mod persistence;
mod sed_awk;
mod shell_lex;

pub(crate) use main_checkout::{
    evaluate_main_checkout_commit_command, evaluate_main_checkout_destructive_command,
};
pub(crate) use persistence::command_is_persistence_only;

use std::path::{Path, PathBuf};

use crate::commands::hook_rewrite::{effective_tool_name, first_command_token};
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
    for segment in split_shell_segments(trimmed) {
        if let Some(reason) = classify_bash_segment(segment, depth) {
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
fn split_shell_segments(command: &str) -> Vec<&str> {
    let scan = QuoteScan::new(command);
    let quoted = |i: usize| scan.balanced && !scan.is_unquoted(i);
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if quoted(i) {
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
        match program {
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
            _ => {}
        }
    }
    if effective_tool_name(trimmed) == "npm test" {
        return Some(BUILD_TEST_REASON);
    }
    classify_command_substitutions(trimmed, depth)
}

/// Inspect `$(…)` / backtick command substitutions for hidden forbidden verbs.
///
/// Why: first-token classification sees the *outer* command only, so
/// `echo "$(sed -i s/a/b/ f)"` would pass on `echo` while the substitution
/// silently runs `sed`. Since the dangerous direction here is a MISSED deny,
/// this scans substitutions too. Design choice: rather than blanket-deny every
/// substitution (which would break trivial, ubiquitous forms like
/// `echo "$(date)"`), it recursively classifies the *body* of each with
/// [`evaluate_bash_command_inner`] — benign body allows, forbidden one denies.
/// Unbalanced substitutions (an opening `$(` / backtick with no matching close)
/// deny conservatively, and so does over-deep nesting: an adversarial
/// `$($($(…` chain is bounded by [`MAX_SUBSTITUTION_DEPTH`] so it can never
/// exhaust the stack and crash the guard process (a deny is the safe outcome).
/// What: past the depth cap, returns [`SHELL_EDIT_REASON`] immediately.
/// Otherwise byte-scans for `$(` (matching `)` with paren-depth tracking) and
/// backtick pairs, recursively evaluates each balanced body at `depth + 1`, and
/// propagates a deny; unbalanced substitutions return [`SHELL_EDIT_REASON`].
/// Quote-aware (#2734): a `$(`/backtick inside SINGLE quotes is literal text
/// (`git commit -m 'costs $(x)'`) and is skipped — but double quotes do NOT
/// suppress substitution (`echo "$(sed -i …)"` still runs `sed`), so those stay
/// scanned. On unbalanced quotes the map is untrustworthy and every position is
/// scanned (the original conservative behaviour).
/// Test: `evaluate_bash_command_denies_hidden_substitution_verb`,
/// `evaluate_bash_command_allows_benign_substitution`,
/// `evaluate_bash_command_denies_unbalanced_substitution`,
/// `evaluate_bash_command_bounds_deep_substitution_nesting`,
/// `evaluate_bash_command_allows_quoted_substitution_prose`.
fn classify_command_substitutions(segment: &str, depth: usize) -> Option<&'static str> {
    if depth >= MAX_SUBSTITUTION_DEPTH {
        // Too deeply nested to safely decompose — deny conservatively rather
        // than recurse further and risk a stack overflow.
        return Some(SHELL_EDIT_REASON);
    }
    let scan = QuoteScan::new(segment);
    let live = |i: usize| !scan.balanced || scan.allows_substitution(i);
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !live(i) {
            i += 1;
            continue;
        }
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
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
                // Unbalanced `$(` — cannot decompose; deny conservatively.
                return Some(SHELL_EDIT_REASON);
            }
            if let Some(reason) = evaluate_bash_command_inner(&segment[i + 2..j - 1], depth + 1) {
                return Some(reason);
            }
            i = j;
            continue;
        }
        if bytes[i] == b'`' {
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
/// What: scans each composition segment ([`split_shell_segments`]) for either
/// a real file-write redirect ([`redirection_target`]) or, for a
/// sed/awk-family/`patch`/`git apply` segment, the command's trailing
/// non-flag token ([`trailing_file_token`]) — the conventional position of the
/// target file for those verbs. Returns the first match found; `None` when no
/// segment yields a plausible target (the caller then falls back to the
/// generic delegation hint). This is a best-effort HINT only — it never
/// affects the allow/deny decision, only which agent name a denial message
/// suggests.
/// Test: `extract_shell_edit_target_*`.
pub(crate) fn extract_shell_edit_target(command: &str) -> Option<String> {
    for segment in split_shell_segments(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(target) = redirection_target(trimmed) {
            return Some(target);
        }
        if let Some(program) = first_command_token(trimmed) {
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

/// The real file-write redirect target in `command`, if any (owned-string
/// sibling of [`has_file_write_redirection`], for routing-hint extraction
/// rather than a pure yes/no classification).
///
/// What: same scan rules as [`has_file_write_redirection`] — skips fd-dups
/// (`>&`, `2>&1`) and `/dev/null` discards — but returns the target token
/// itself the first time a real file-write redirect is found, instead of a
/// bool.
/// Test: `extract_shell_edit_target_from_redirection`.
fn redirection_target(command: &str) -> Option<String> {
    let scan = QuoteScan::new(command);
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if scan.balanced && !scan.is_unquoted(i) {
            i += 1;
            continue;
        }
        if bytes[i] == b'>' {
            let mut j = i + 1;
            if j < bytes.len() && bytes[j] == b'>' {
                j += 1;
            }
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'&' {
                i = j + 1;
                continue;
            }
            let start = j;
            while j < bytes.len() && !matches!(bytes[j], b' ' | b'>' | b'<' | b'|' | b';' | b'&') {
                j += 1;
            }
            let target = &command[start..j];
            if target == "/dev/null" || target.is_empty() {
                i = j;
                continue;
            }
            return Some(target.to_string());
        }
        i += 1;
    }
    None
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
/// What: scans for `>`; for each, skips a second `>` (append) and any spaces,
/// then treats it as an fd-duplication (allow, keep scanning) only when the
/// next non-space byte is `&`. It then reads the redirect *target* token and
/// treats `/dev/null` as benign (output discard, e.g. `2>/dev/null` /
/// `>/dev/null` / `&>/dev/null`) — allow, keep scanning. Any other `>` is a
/// file-write redirect → `true`.
/// Quote-aware (#2734): a `>` inside a quoted string is literal argument content
/// (`git commit -m 'spec -> code'` — Bob's live false positive), not a
/// redirection, and is skipped — UNLESS the command's quotes are unbalanced, in
/// which case the [`QuoteScan`] map is untrustworthy and every `>` is scanned
/// (the original conservative, over-blocking behaviour). Shell-level redirects
/// (`echo x > f`) are unquoted and still caught. NOTE: an in-quote `>` that is
/// genuinely dangerous — an `awk 'BEGIN{print > "f"}'` in-program file write —
/// is caught by [`sed_awk::awk_is_readonly`], not here.
/// Test: `has_file_write_redirection_detects_write`,
/// `has_file_write_redirection_detects_append`,
/// `has_file_write_redirection_ignores_fd_dup`,
/// `has_file_write_redirection_ignores_dev_null`,
/// `has_file_write_redirection_ignores_quoted_gt`,
/// `has_file_write_redirection_false_for_plain_command`.
pub(crate) fn has_file_write_redirection(command: &str) -> bool {
    let scan = QuoteScan::new(command);
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if scan.balanced && !scan.is_unquoted(i) {
            i += 1;
            continue;
        }
        if bytes[i] == b'>' {
            let mut j = i + 1;
            // `>>` append is still a file write; skip the second `>`.
            if j < bytes.len() && bytes[j] == b'>' {
                j += 1;
            }
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            // `>&fd` / `2>&1` duplicate a descriptor — not a file write.
            if j < bytes.len() && bytes[j] == b'&' {
                i = j + 1;
                continue;
            }
            // Read the redirect target token. `/dev/null` is an output-discard
            // sink, not a file write (`which cargo 2>/dev/null`,
            // `command -v foo >/dev/null`) — allow it and keep scanning.
            let start = j;
            while j < bytes.len() && !matches!(bytes[j], b' ' | b'>' | b'<' | b'|' | b';' | b'&') {
                j += 1;
            }
            if &command[start..j] == "/dev/null" {
                i = j;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
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
/// under on macOS (e.g. `/var/folders/x1/…/T/`), and the harness scratchpad
/// (`/private/tmp/claude-502/…`) is already covered by the `/private/tmp`
/// entry — it is called out separately in the deny message (not here) because
/// agents are told to prefer it "instead of /tmp", making it the subtle case
/// that looks compliant while still landing in a denylisted zone.
const WORKTREE_TMP_DENYLIST_ROOTS: &[&str] = &["/tmp", "/private/tmp", "/var/folders"];

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
///   `/tmp` itself being a symlink to `/private/tmp` is NOT in this bucket —
///   both spellings are literal entries in [`WORKTREE_TMP_DENYLIST_ROOTS`],
///   so a literal `/tmp/...` argument is caught without needing to resolve
///   the symlink.
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

/// The environment values [`resolve_target_path`] expands `$TMPDIR`/`$TMP`/`~`
/// from, captured as data instead of read from `std::env` at each use.
///
/// Why: the expansion rules can only be tested by controlling those variables,
/// and the obvious way to do that — `std::env::set_var` in the test — mutates
/// PROCESS-GLOBAL state that every other test in the `tm` test binary sees for
/// as long as it is set. `cargo test` runs tests as threads in one process, so
/// a restore-on-drop guard bounds the leak's lifetime but not its visibility:
/// concurrent siblings still read the mutated value. That is not hypothetical —
/// a `TMPDIR` pinned to a macOS-only scratch path reddened five
/// `pm_guard_budget` tests on a Linux CI runner (PR #4914, run 31023632348)
/// because `tempfile` honors `$TMPDIR` and the path does not exist there.
/// Passing the values in removes the global mutation rather than scheduling
/// around it.
/// What: the three variables [`resolve_target_path`] expands, each `None` when
/// unset. [`PathEnv::from_process`] is the one place that reads the real
/// environment, so production behavior is unchanged — a `Bash` tool call
/// inherits the guard process's environment, and the guard expands against it.
/// Test: `evaluate_worktree_add_command_expands_tmpdir_and_home` builds one
/// directly; every other caller goes through [`PathEnv::from_process`].
struct PathEnv {
    tmpdir: Option<String>,
    tmp: Option<String>,
    home: Option<String>,
}

impl PathEnv {
    /// Read `$TMPDIR`, `$TMP`, and `$HOME` from the guard process.
    fn from_process() -> Self {
        Self {
            tmpdir: std::env::var("TMPDIR").ok(),
            tmp: std::env::var("TMP").ok(),
            home: std::env::var("HOME").ok(),
        }
    }
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
        let resolved = resolve_target_path(&target, &base, env);
        if resolves_under_denylisted_tmp(&resolved) {
            return Some(WORKTREE_TMP_REASON);
        }
    }
    None
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

/// Expand a leading `~`, `$TMPDIR`/`${TMPDIR}`, and `$TMP`/`${TMP}` in a path
/// token using `env`, then resolve it against `base` if relative, then
/// lexically normalize (collapse `.`/`..` components WITHOUT touching the
/// filesystem).
///
/// Why: `shlex::split` does not perform shell variable expansion, so a target
/// argument like `$TMPDIR/wt-foo` or `~/scratch/wt-foo` reaches this function
/// as literal text; the guard must expand it itself to see where it really
/// points. Filesystem-touching resolution (`fs::canonicalize`) is deliberately
/// avoided — the worktree target usually does not exist yet, and a
/// `PreToolUse` hook must stay fast and side-effect-free.
/// What: string-replaces the env-var forms, expands a `~`/`~/…` prefix via
/// `$HOME`, joins onto `base` if the result is still relative, then
/// normalizes. The values arrive via [`PathEnv`] rather than being read here,
/// so a test can pin them without mutating process-global state — see
/// [`PathEnv`] for the CI failure that motivated the seam. In production
/// [`PathEnv::from_process`] supplies the guard process's own environment,
/// which a `Bash` tool call inherits unchanged.
fn resolve_target_path(token: &str, base: &Path, env: &PathEnv) -> PathBuf {
    let mut expanded = token.to_string();
    if let Some(tmpdir) = env.tmpdir.as_deref() {
        expanded = expanded
            .replace("${TMPDIR}", tmpdir)
            .replace("$TMPDIR", tmpdir);
    }
    if let Some(tmp) = env.tmp.as_deref() {
        expanded = expanded.replace("${TMP}", tmp).replace("$TMP", tmp);
    }
    if expanded == "~" {
        if let Some(home) = env.home.as_deref() {
            expanded = home.to_string();
        }
    } else if let Some(rest) = expanded.strip_prefix("~/")
        && let Some(home) = env.home.as_deref()
    {
        expanded = format!("{home}/{rest}");
    }
    let path = Path::new(&expanded);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_lexically(&joined)
}

/// Collapse `.`/`..` path components without touching the filesystem.
///
/// Why: [`resolve_target_path`] must not call `fs::canonicalize` (the target
/// usually doesn't exist yet), but a purely textual join like
/// `/repo/../tmp/wt` must still be recognized as resolving under `/tmp`.
/// What: walks `path`'s components, popping the accumulator on `..` and
/// dropping `.`, otherwise appending. Does not consult the filesystem, so it
/// cannot see through symlinks (see the residual-bypass note on
/// [`evaluate_worktree_add_command`]).
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `path` lexically starts with one of [`WORKTREE_TMP_DENYLIST_ROOTS`].
fn resolves_under_denylisted_tmp(path: &Path) -> bool {
    WORKTREE_TMP_DENYLIST_ROOTS
        .iter()
        .any(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests;
