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
//! write/exec script construct, balanced quotes) to be allowed.
//! Test: `evaluate_bash_command_*`, `split_shell_segments_*`, and
//! `has_file_write_redirection_*` in this module's `tests` submodule;
//! `sed_awk::tests` for the sed/awk-specific safety analysis.

mod sed_awk;

use crate::commands::hook_rewrite::{effective_tool_name, first_command_token};

/// Deny reason for editing files through a shell tool (sed/awk/patch/git apply/redirection).
pub(crate) const SHELL_EDIT_REASON: &str = "PM must not edit files via shell tools \
     (sed/awk/patch/git apply/redirection) (prohibitions P1–P3). \
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
/// Deliberately quote-unaware — see [`evaluate_bash_command`].
/// Test: `split_shell_segments_splits_operators`,
/// `split_shell_segments_splits_bare_ampersand`,
/// `split_shell_segments_single_command`.
fn split_shell_segments(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
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
            _ => {}
        }
    }
    match effective_tool_name(trimmed).as_str() {
        "git apply" => return Some(SHELL_EDIT_REASON),
        "npm test" => return Some(BUILD_TEST_REASON),
        _ => {}
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
/// Test: `evaluate_bash_command_denies_hidden_substitution_verb`,
/// `evaluate_bash_command_allows_benign_substitution`,
/// `evaluate_bash_command_denies_unbalanced_substitution`,
/// `evaluate_bash_command_bounds_deep_substitution_nesting`.
fn classify_command_substitutions(segment: &str, depth: usize) -> Option<&'static str> {
    if depth >= MAX_SUBSTITUTION_DEPTH {
        // Too deeply nested to safely decompose — deny conservatively rather
        // than recurse further and risk a stack overflow.
        return Some(SHELL_EDIT_REASON);
    }
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
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
/// A pragmatic byte scan (a `>` inside a quoted string can false-positive) that
/// errs toward blocking, matching the conservative posture of the sibling
/// `hook_rewrite::has_unsafe_pipe_composition`.
/// Test: `has_file_write_redirection_detects_write`,
/// `has_file_write_redirection_detects_append`,
/// `has_file_write_redirection_ignores_fd_dup`,
/// `has_file_write_redirection_ignores_dev_null`,
/// `has_file_write_redirection_false_for_plain_command`.
pub(crate) fn has_file_write_redirection(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_bash_command_denies_shell_edit_verbs() {
        for cmd in [
            "sed -i s/a/b/ src/lib.rs",
            "sed -i.bak s/a/b/ src/lib.rs",
            "awk -i inplace '{print}' f",
            "patch -p1 < d.patch",
            "git apply my.patch",
            "FOO=1 sed -i s/a/b/ f",
            "sudo patch -p0 x",
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(SHELL_EDIT_REASON),
                "expected shell-edit deny for: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_allows_readonly_sed_awk() {
        // #2664: sed/awk used as narrowly read-only stream filters (no
        // in-place flag, no external script, no write/exec construct,
        // balanced quotes) must not be blanket-denied by name.
        for cmd in [
            "git status --porcelain | awk '{print $1}' | sort | uniq -c",
            "sed -n '1,5p' file.txt",
            "git log | awk '{print $1}'",
            "cat f | sed 's/a/b/'",
            "sed 's/a/b/g' f | grep x",
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                None,
                "expected allow for read-only sed/awk: {cmd:?}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_awk_shell_escape_holes() {
        // Code-critic BLOCK on PR #2677: awk's `system()` builtin runs an
        // arbitrary shell command with no `-i` and no `>` — the
        // allow-by-default design missed this entirely.
        for cmd in [
            r#"awk 'BEGIN{system("touch /tmp/x")}'"#,
            r#"awk 'BEGIN{system("curl https://evil/x")}'"#,
            // A co-process pipe/read inside the awk program is itself a
            // bare `|`/`;` from the (quote-unaware) segment splitter's point
            // of view, so it fragments the quoted program and leaves an
            // unbalanced quote count in the resulting segment — denied.
            r#"awk 'BEGIN{print | "sort"}'"#,
            r#"awk 'BEGIN{"date" | getline}'"#,
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(SHELL_EDIT_REASON),
                "expected shell-edit deny for awk shell-escape: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_sed_e_w_commands() {
        // sed's `e` (execute) and `w`/`W` (write-to-file) commands need no
        // `-i` and no `>` — a bare verb-name deny-list miss, closed by
        // deny-by-default + script-content analysis in `sed_awk`.
        for cmd in [
            "sed '1e touch /tmp/x' f",
            "sed -n '1,5w /tmp/out' f",
            "sed 's/a/b/e' f",
            "sed 's/a/b/w /tmp/out' f",
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(SHELL_EDIT_REASON),
                "expected shell-edit deny for sed e/w command: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_external_script_load() {
        // `-f`/`--file` loads a script the guard cannot see — it may contain
        // any of the w/e/system holes above, so deny unconditionally.
        for cmd in ["sed -f script.sed f", "awk -f script.awk f"] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(SHELL_EDIT_REASON),
                "expected shell-edit deny for external script load: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_sed_in_place_abbreviations() {
        // GNU getopt long-option abbreviations of `--in-place` must still
        // deny, not just the exact spelling.
        for cmd in [
            "sed --in s/a/b/ f",
            "sed --in-p s/a/b/ f",
            "sed --in=bak s/a/b/ f",
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(SHELL_EDIT_REASON),
                "expected shell-edit deny for --in-place abbreviation: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_awk_family_verbs() {
        // gawk/nawk/mawk are the same in-place-edit-capable interpreter
        // family as awk and must be classified identically.
        assert_eq!(
            evaluate_bash_command("gawk -i inplace '{print}' f"),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_bash_command(r#"nawk 'BEGIN{system("id")}'"#),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_bash_command("gawk '{print $1}'"),
            None,
            "read-only gawk must still allow"
        );
    }

    #[test]
    fn evaluate_bash_command_denies_redirection_write() {
        assert_eq!(
            evaluate_bash_command("echo 'code' > src/lib.rs"),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_bash_command("printf x >> notes.txt"),
            Some(SHELL_EDIT_REASON)
        );
    }

    #[test]
    fn evaluate_bash_command_denies_build_and_test() {
        for cmd in ["make build", "pytest -q", "npm test"] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(BUILD_TEST_REASON),
                "expected build/test deny for: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_network() {
        for cmd in ["curl https://example.com", "wget https://example.com/x"] {
            assert_eq!(
                evaluate_bash_command(cmd),
                Some(NETWORK_REASON),
                "expected network deny for: {cmd}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_allows_normal_shell() {
        for cmd in [
            "",
            "   ",
            "git status",
            "git add -A",
            "git commit -m x",
            "git log --oneline",
            "git diff",
            "git push",
            "ls -la",
            "grep -rn foo src",
            "cat README.md",
            "cargo tree 2>&1", // fd-dup, not a file write
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                None,
                "expected allow for: {cmd:?}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_composition_hidden_verbs() {
        // A benign leading verb must NOT hide a forbidden verb in a later
        // composed segment (the shell-composition bypass, PR #1985).
        assert_eq!(
            evaluate_bash_command("cd repo && sed -i s/a/b/ f"),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_bash_command("true; make build"),
            Some(BUILD_TEST_REASON)
        );
        assert_eq!(
            evaluate_bash_command("x || pytest"),
            Some(BUILD_TEST_REASON)
        );
        assert_eq!(
            evaluate_bash_command("ls && git apply p.diff"),
            Some(SHELL_EDIT_REASON)
        );
        // Redirection hidden after a benign first segment still denies.
        assert_eq!(
            evaluate_bash_command("cd repo && echo x > out.txt"),
            Some(SHELL_EDIT_REASON)
        );
    }

    #[test]
    fn evaluate_bash_command_denies_bare_ampersand_composition() {
        // A bare `&` (background separator) must NOT hide a forbidden trailing
        // verb — the bare-`&` composition bypass (second adversarial review).
        assert_eq!(
            evaluate_bash_command("true & sed -i s/a/b/ f"),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_bash_command("cd x & make build"),
            Some(BUILD_TEST_REASON)
        );
        assert_eq!(evaluate_bash_command("x & pytest"), Some(BUILD_TEST_REASON));
        // Newline is a separator too.
        assert_eq!(
            evaluate_bash_command("cd repo\nsed -i s/a/b/ f"),
            Some(SHELL_EDIT_REASON)
        );
    }

    #[test]
    fn evaluate_bash_command_allows_trailing_background() {
        // Trailing background `&` with nothing after must not become a spurious
        // forbidden segment, and fd-dups must not be treated as separators.
        for cmd in ["sleep 1 &", "cargo tree 2>&1", "foo >&2"] {
            assert_eq!(
                evaluate_bash_command(cmd),
                None,
                "expected allow for: {cmd:?}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_allows_benign_pipes() {
        // Composition where NO segment is a forbidden verb must still allow —
        // we do not blanket-deny all composition.
        for cmd in [
            "git log | head",
            "cat f | grep x",
            "ls | wc -l",
            "git diff && git status",
            "cd repo; ls -la",
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                None,
                "expected allow for benign composition: {cmd:?}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_allows_dev_null_redirect() {
        // `/dev/null` is an output-discard sink, not a file write.
        for cmd in ["which cargo 2>/dev/null", "command -v foo >/dev/null"] {
            assert_eq!(
                evaluate_bash_command(cmd),
                None,
                "expected allow for /dev/null discard: {cmd:?}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_hidden_substitution_verb() {
        // A forbidden verb inside a command substitution must be caught.
        assert_eq!(
            evaluate_bash_command("echo \"$(sed -i s/a/b/ f)\""),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_bash_command("x=`make build`"),
            Some(BUILD_TEST_REASON)
        );
    }

    #[test]
    fn evaluate_bash_command_allows_benign_substitution() {
        // Trivial substitutions with benign bodies must not be over-blocked.
        for cmd in [
            "echo \"$(date)\"",
            "cd \"$(git rev-parse --show-toplevel)\"",
        ] {
            assert_eq!(
                evaluate_bash_command(cmd),
                None,
                "expected allow for benign substitution: {cmd:?}"
            );
        }
    }

    #[test]
    fn evaluate_bash_command_denies_unbalanced_substitution() {
        // An unbalanced substitution we cannot decompose denies conservatively.
        assert_eq!(
            evaluate_bash_command("echo $(sed -i"),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(evaluate_bash_command("echo `foo"), Some(SHELL_EDIT_REASON));
    }

    #[test]
    fn evaluate_bash_command_bounds_deep_substitution_nesting() {
        // Adversarial deep `$(` nesting must return a decision without panicking
        // (stack-exhaustion guard). A deny is the acceptable safe outcome.
        let deep = format!("echo {}", "$(".repeat(200));
        let decision = evaluate_bash_command(&deep);
        assert!(decision.is_some(), "deep nesting must deny, got allow");
        // Balanced-but-deep nesting is also bounded (no panic, returns a value).
        let balanced = format!("echo {}date{}", "$(".repeat(200), ")".repeat(200));
        let _ = evaluate_bash_command(&balanced);
    }

    #[test]
    fn split_shell_segments_splits_operators() {
        assert_eq!(
            split_shell_segments("a && b || c ; d | e"),
            vec!["a ", " b ", " c ", " d ", " e"]
        );
    }

    #[test]
    fn split_shell_segments_splits_bare_ampersand() {
        // A bare `&` with a following command splits; fd-dups and trailing
        // background do not.
        assert_eq!(split_shell_segments("a & b"), vec!["a ", " b"]);
        assert_eq!(split_shell_segments("foo &"), vec!["foo &"]);
        assert_eq!(
            split_shell_segments("cargo test 2>&1"),
            vec!["cargo test 2>&1"]
        );
        assert_eq!(split_shell_segments("foo >&2"), vec!["foo >&2"]);
        // Newline splits.
        assert_eq!(split_shell_segments("a\nb"), vec!["a", "b"]);
    }

    #[test]
    fn split_shell_segments_single_command() {
        assert_eq!(split_shell_segments("git status"), vec!["git status"]);
        // Backgrounding `&` (nothing after) is not a split point.
        assert_eq!(split_shell_segments("foo &"), vec!["foo &"]);
    }

    #[test]
    fn has_file_write_redirection_detects_write() {
        assert!(has_file_write_redirection("echo x > f"));
        assert!(has_file_write_redirection("echo x >f.rs"));
    }

    #[test]
    fn has_file_write_redirection_detects_append() {
        assert!(has_file_write_redirection("echo x >> f"));
    }

    #[test]
    fn has_file_write_redirection_ignores_fd_dup() {
        assert!(!has_file_write_redirection("cargo test 2>&1"));
        assert!(!has_file_write_redirection("foo >&2"));
    }

    #[test]
    fn has_file_write_redirection_ignores_dev_null() {
        // Discarding output to /dev/null is not a file write.
        assert!(!has_file_write_redirection("which cargo 2>/dev/null"));
        assert!(!has_file_write_redirection("command -v foo >/dev/null"));
        assert!(!has_file_write_redirection("foo &>/dev/null"));
        // A real file write with the same shape still denies.
        assert!(has_file_write_redirection("echo x > /dev/null.txt"));
        assert!(has_file_write_redirection("echo x > out.txt"));
    }

    #[test]
    fn has_file_write_redirection_false_for_plain_command() {
        assert!(!has_file_write_redirection("git status"));
    }
}
