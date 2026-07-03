//! `tm hook --pm-guard` — PreToolUse enforcement for MPM-managed PM sessions
//! (issue #1977).
//!
//! Why: the deployed PM instructions carry prohibitions P1–P11 ("delegate all
//! file edits to rust-engineer", "never run builds/tests yourself", …) but
//! they are prompt-only — nothing stops the model from calling `Edit`/`Write`
//! or shelling out to `sed`/`make`/`pytest` directly. Claude Code's
//! `PreToolUse` hook is the single mechanical seam that can *block* a tool call
//! before it runs, so wiring a guard here turns those prohibitions from advice
//! into enforcement while still steering the PM toward the Task/Agent tool.
//! What: [`pm_guard`] is the `tm hook --pm-guard` entry point. It short-circuits
//! (ALLOW) inside sub-agents / disabled-hook shells / the explicit
//! `TRUSTY_MPM_PM_UNRESTRICTED=1` bypass, reads the `PreToolUse` stdin payload,
//! classifies the tool **locally** (no daemon round-trip — a PreToolUse hook
//! must be fast and must never hard-block when the daemon is down), and either
//! stays silent (ALLOW) or prints a `permissionDecision: "deny"` response
//! (DENY). The classification is a static default-ALLOW + explicit deny-list:
//! [`evaluate_tool`] denies the file-edit tools and forbidden Bash verbs and
//! allows everything else. Every error path (malformed stdin, missing fields)
//! fails **open** — ALLOW — so a broken hook never wedges the PM.
//! Test: `evaluate_*`, `has_file_write_redirection_*`, and
//! `build_pretooluse_deny_response_*` below cover the policy + JSON shape;
//! `tests/tm_hook_pm_guard.rs` exercises the stdin→decision→stdout path (and
//! the env bypasses / fail-open) end to end through the real binary.

use crate::commands::hook_rewrite::{effective_tool_name, first_command_token};
use crate::commands::misc::{DISABLE_HOOKS_ENV, SUB_AGENT_ENV, read_stdin_hook_payload};

/// Escape-hatch env var: `TRUSTY_MPM_PM_UNRESTRICTED=1` disables all PM
/// enforcement for the invocation.
///
/// Why: the prohibitions exist for the *autonomous* PM loop; when the operator
/// explicitly tells the PM "you do it this time", there must be a way to lift
/// the block without editing settings.json and restarting. Keying it on the
/// exact value `"1"` (rather than mere presence) keeps an accidental empty
/// export from silently disabling enforcement.
/// What: the literal env var name. See [`pm_unrestricted`].
pub(crate) const PM_UNRESTRICTED_ENV: &str = "TRUSTY_MPM_PM_UNRESTRICTED";

/// File-editing tools the PM must delegate to rust-engineer (prohibition P1).
///
/// Why: these are the direct filesystem-mutation tools; the PM's job is to
/// dispatch them to an engineering sub-agent via the Task/Agent tool, never to
/// wield them itself.
/// What: matched by exact tool name in [`evaluate_tool`].
const DENIED_EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Deny reason for a direct file-edit tool call.
const EDIT_TOOL_REASON: &str = "PM must not edit files directly (prohibition P1). \
     Delegate the change to rust-engineer via the Task/Agent tool.";

/// Deny reason for editing files through a shell tool (sed/awk/patch/git apply/redirection).
const SHELL_EDIT_REASON: &str = "PM must not edit files via shell tools \
     (sed/awk/patch/git apply/redirection) (prohibitions P1–P3). \
     Delegate the change to rust-engineer via the Task/Agent tool.";

/// Deny reason for running builds/tests directly (make/pytest/npm test).
const BUILD_TEST_REASON: &str = "PM must not run builds or tests directly \
     (prohibitions P4–P5). Delegate to rust-engineer / QA via the Task/Agent tool.";

/// Deny reason for fetching over the network from Bash (curl/wget).
const NETWORK_REASON: &str = "PM must not fetch over the network from Bash \
     (prohibition P-network). Use WebFetch/WebSearch or delegate the task.";

/// `tm hook --pm-guard` entry point — classify a `PreToolUse` call and emit an
/// ALLOW (silent) or DENY (`permissionDecision`) response.
///
/// Why: this is the load-bearing integration point wired into every managed
/// session's `PreToolUse` hook (see `session_launch::settings::write_project_hooks`).
/// It must be fast and fail-open: guard env vars short-circuit to ALLOW so
/// nested sub-agents (`CLAUDE_MPM_SUB_AGENT`) and CI / build shells
/// (`TRUSTY_MPM_DISABLE_HOOKS`) are never blocked, and the explicit
/// `TRUSTY_MPM_PM_UNRESTRICTED=1` bypass lets the operator lift enforcement on
/// demand. Any missing/malformed input degrades to ALLOW rather than wedging
/// the PM. The decision is computed purely from the static tool classification
/// — never a daemon call — so a down daemon can never hard-block a tool call.
/// What: returns `Ok(())` (ALLOW, no stdout) for every short-circuit, a payload
/// with no `tool_name`, or an [`evaluate_tool`] verdict of ALLOW. On DENY it
/// fires a best-effort audit POST to `<url>/hooks` (tight timeout, result
/// ignored — audit only, never gating) and prints the
/// `hookSpecificOutput.permissionDecision = "deny"` JSON to stdout, then
/// returns. `url` is used only for the audit POST.
/// Test: env short-circuits + fail-open + deny/allow are covered end to end in
/// `tests/tm_hook_pm_guard.rs`; the pure policy is covered by this module's
/// unit tests.
pub(crate) async fn pm_guard(url: &str) -> anyhow::Result<()> {
    // Guard 1: never block a nested MPM sub-agent — it is doing the delegated
    // work the PM is being steered toward.
    if std::env::var_os(SUB_AGENT_ENV).is_some() {
        return Ok(());
    }
    // Guard 2: universal opt-out for CI / build shells that can't edit
    // settings.json without a restart.
    if std::env::var_os(DISABLE_HOOKS_ENV).is_some() {
        return Ok(());
    }
    // Guard 3: explicit operator override — "the user said you do it".
    if pm_unrestricted() {
        return Ok(());
    }

    // Fail-open: no stdin payload at all → ALLOW.
    let Some(payload) = read_stdin_hook_payload().await else {
        return Ok(());
    };
    // Fail-open: a payload with no `tool_name` we can classify → ALLOW.
    let Some(tool_name) = payload.get("tool_name").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    let tool_input = payload.get("tool_input");

    let Some(reason) = evaluate_tool(tool_name, tool_input) else {
        // ALLOW: exit 0 with no output so the normal permission flow applies.
        return Ok(());
    };

    // DENY. Record the enforcement action (best-effort, never gating), then
    // emit the block. The audit POST is intentionally the *only* daemon call
    // and fires solely on the rare deny path, so the common ALLOW path adds
    // zero latency to every tool invocation.
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    audit_denied_tool(url, session_id, tool_name, reason).await;
    println!("{}", build_pretooluse_deny_response(reason));
    Ok(())
}

/// Whether the `TRUSTY_MPM_PM_UNRESTRICTED=1` bypass is active.
///
/// Why: split out so the exact-`"1"` matching rule has one definition.
/// What: `true` only when [`PM_UNRESTRICTED_ENV`] is set to exactly `"1"`.
/// Test: exercised via `tests/tm_hook_pm_guard.rs::pm_guard_bypass_env_allows_all`.
fn pm_unrestricted() -> bool {
    std::env::var(PM_UNRESTRICTED_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Classify a `PreToolUse` tool call: `Some(reason)` denies, `None` allows.
///
/// Why: the default-ALLOW + explicit deny-list policy. Default-deny would
/// break the PM (it legitimately calls Read/Grep/Task/git-status/mcp tools all
/// the time); only the small, high-confidence set of prohibited actions is
/// blocked.
/// What: denies the [`DENIED_EDIT_TOOLS`] by exact name, delegates a `Bash`
/// call to [`evaluate_bash_command`] against its `command` argument, and allows
/// every other tool (Read, Grep, Glob, Task, TodoWrite, WebFetch, `mcp__*`, …).
/// Test: `evaluate_tool_denies_edit_tools`, `evaluate_tool_allows_read_and_task`,
/// `evaluate_tool_delegates_bash`.
pub(crate) fn evaluate_tool(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<&'static str> {
    if DENIED_EDIT_TOOLS.contains(&tool_name) {
        return Some(EDIT_TOOL_REASON);
    }
    if tool_name == "Bash" {
        let command = tool_input
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        return evaluate_bash_command(command);
    }
    None
}

/// Classify a `Bash` command: `Some(reason)` denies, `None` allows.
///
/// Why: Bash is the escape hatch a prompt-only prohibition can't close — a PM
/// can edit a file with `sed -i`, write one with `echo … > f.rs`, apply a
/// diff with `git apply`, or run the suite with `pytest` — all of which
/// side-step the P1–P5 prohibitions. Crucially, shell *composition* is itself a
/// bypass: classifying only the first token of the whole line let
/// `cd repo && sed -i …`, `true; make build`, and `x || pytest` slip through
/// because their leading verb (`cd`/`true`/`x`) is benign. So the classifier
/// must look at every composed command, not just the first.
/// What: splits the command on the shell composition operators `&&`, `||`, `;`,
/// and `|` (see [`split_shell_segments`]) and runs [`classify_bash_segment`] on
/// each segment — denying if ANY segment names a forbidden verb — then applies
/// the whole-command file-write redirection check ([`has_file_write_redirection`],
/// which ignores `2>&1` fd-dups and `/dev/null` discards). Benign pipes whose
/// segments are all allowed (`git log | head`, `ls | wc -l`) still pass. Empty
/// commands allow. The split is deliberately quote-unaware: a forbidden verb
/// hidden inside a quoted string may over-deny, which is the safe direction here
/// (a missed deny is the dangerous one).
/// Test: `evaluate_bash_command_denies_*`, `evaluate_bash_command_allows_*`,
/// `evaluate_bash_command_denies_composition_*`.
pub(crate) fn evaluate_bash_command(command: &str) -> Option<&'static str> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    for segment in split_shell_segments(trimmed) {
        if let Some(reason) = classify_bash_segment(segment) {
            return Some(reason);
        }
    }
    if has_file_write_redirection(trimmed) {
        return Some(SHELL_EDIT_REASON);
    }
    None
}

/// Split a command into the sub-commands joined by `&&`/`||`/`;`/`|`.
///
/// Why: a forbidden verb can hide in any composed segment, not just the first;
/// splitting lets [`evaluate_bash_command`] classify each independently.
/// What: a byte scan that cuts at each composition operator (preferring the
/// two-byte `&&`/`||` over a bare `|`) and returns the raw (untrimmed) segments;
/// `classify_bash_segment` trims. Deliberately quote-unaware — see
/// [`evaluate_bash_command`]. A lone `&` (backgrounding) is NOT a split point,
/// so `foo &` stays one segment.
/// Test: `split_shell_segments_splits_operators`,
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
        if bytes[i] == b';' || bytes[i] == b'|' {
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
/// Why: factored out of [`evaluate_bash_command`] so the same first-token /
/// two-token / substitution deny logic runs uniformly on every segment.
/// What: resolves the effective program via [`first_command_token`] (strips
/// env/`sudo`/path noise) and denies `sed`/`awk`/`patch` (shell edit),
/// `curl`/`wget` (network), `make`/`pytest` (build/test); matches the two-token
/// forms `git apply` / `npm test` via [`effective_tool_name`]; and finally
/// inspects any command substitution / subshell in the segment via
/// [`classify_command_substitutions`]. Empty/whitespace segments allow.
/// Test: covered via `evaluate_bash_command_*` (this is its per-segment core).
fn classify_bash_segment(segment: &str) -> Option<&'static str> {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(program) = first_command_token(trimmed) {
        match program {
            "sed" | "awk" | "patch" => return Some(SHELL_EDIT_REASON),
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
    classify_command_substitutions(trimmed)
}

/// Inspect `$(…)` / backtick command substitutions for hidden forbidden verbs.
///
/// Why: first-token classification sees the *outer* command only, so
/// `echo "$(sed -i s/a/b/ f)"` would pass on `echo` while the substitution
/// silently runs `sed`. Since the dangerous direction here is a MISSED deny,
/// this scans substitutions too. Design choice: rather than blanket-deny every
/// substitution (which would break trivial, ubiquitous forms like
/// `echo "$(date)"` or `cd "$(git rev-parse --show-toplevel)"`), it recursively
/// classifies the *body* of each substitution with [`evaluate_bash_command`] —
/// so a benign body allows and a forbidden one denies. Only when a substitution
/// is *unbalanced* (an opening `$(` / backtick with no matching close — a form
/// we cannot confidently decompose, e.g. one broken across a composition split)
/// do we deny conservatively.
/// What: byte-scans for `$(` (matching `)` with paren-depth tracking) and
/// backtick pairs; recursively evaluates each balanced body and propagates a
/// deny; returns [`SHELL_EDIT_REASON`] for any unbalanced substitution. Bounded
/// recursion — each body is strictly shorter than its enclosing string.
/// Test: `evaluate_bash_command_denies_hidden_substitution_verb`,
/// `evaluate_bash_command_allows_benign_substitution`,
/// `evaluate_bash_command_denies_unbalanced_substitution`.
fn classify_command_substitutions(segment: &str) -> Option<&'static str> {
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 {
                // Unbalanced `$(` — cannot decompose; deny conservatively.
                return Some(SHELL_EDIT_REASON);
            }
            if let Some(reason) = evaluate_bash_command(&segment[i + 2..j - 1]) {
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
            if let Some(reason) = evaluate_bash_command(&segment[i + 1..j]) {
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

/// Build the `hookSpecificOutput.permissionDecision = "deny"` JSON body.
///
/// Why: this is the exact shape Claude Code parses on a `PreToolUse` hook's
/// `exit 0` stdout to block a tool call. Confirmed against the live Claude Code
/// hooks reference (<https://code.claude.com/docs/en/hooks>, confirmed
/// 2026-07-03): a deny is
/// `{"hookSpecificOutput":{"hookEventName":"PreToolUse",
/// "permissionDecision":"deny","permissionDecisionReason":"…"}}`, and the
/// documented ALLOW form is simply exit 0 with no JSON — which is why
/// [`pm_guard`] prints nothing on ALLOW rather than emitting a
/// `permissionDecision:"allow"` object. Mirrors
/// `hook_rewrite::build_pretooluse_rewrite_response` (same `hookSpecificOutput`
/// envelope, different decision payload).
/// What: returns the deny `serde_json::Value`; its `Display` impl is compact
/// single-line JSON, so `println!("{}", …)` satisfies the protocol's "stdout
/// must contain only the JSON object" rule.
/// Test: `build_pretooluse_deny_response_has_expected_shape`.
pub(crate) fn build_pretooluse_deny_response(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

/// Best-effort audit POST recording a PM-guard denial to the daemon.
///
/// Why: enforcement actions should be observable (dashboard / audit log), but
/// the audit must never gate or slow the decision — a PreToolUse hook has to
/// be fast and fail-open. So this is fire-and-forget with a tight connect +
/// total timeout and a fully-ignored result; a down/slow daemon costs at most
/// the short timeout and never changes the (already-computed) deny outcome.
/// What: POSTs `{session_id, event:"PreToolUse", payload:{cwd, tool,
/// pm_guard_decision:"deny", pm_guard_reason}}` to `<url>/hooks` and drops any
/// error. Builds its own short-lived client so the connect guard is scoped to
/// this call; a client-build failure silently skips the POST.
/// Test: side-effect-only best-effort I/O — covered indirectly by the deny
/// paths in `tests/tm_hook_pm_guard.rs` (which point at an unreachable daemon,
/// proving the deny still emits despite the failed POST).
async fn audit_denied_tool(url: &str, session_id: &str, tool_name: &str, reason: &str) {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_default();
    let body = serde_json::json!({
        "session_id": session_id,
        "event": "PreToolUse",
        "payload": {
            "cwd": cwd,
            "tool": tool_name,
            "pm_guard_decision": "deny",
            "pm_guard_reason": reason,
        }
    });
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return;
    };
    let _ = client.post(format!("{url}/hooks")).json(&body).send().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_tool_denies_edit_tools() {
        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            assert_eq!(
                evaluate_tool(tool, Some(&serde_json::json!({"file_path": "/x"}))),
                Some(EDIT_TOOL_REASON),
                "expected {tool} to be denied"
            );
        }
    }

    #[test]
    fn evaluate_tool_allows_read_and_task() {
        for tool in [
            "Read",
            "Grep",
            "Glob",
            "Task",
            "TodoWrite",
            "WebFetch",
            "mcp__foo__bar",
        ] {
            assert_eq!(
                evaluate_tool(tool, Some(&serde_json::json!({"x": 1}))),
                None,
                "expected {tool} to be allowed"
            );
        }
    }

    #[test]
    fn evaluate_tool_delegates_bash() {
        // Bash routes through the command classifier.
        assert_eq!(
            evaluate_tool(
                "Bash",
                Some(&serde_json::json!({"command": "sed -i s/a/b/ f"}))
            ),
            Some(SHELL_EDIT_REASON)
        );
        assert_eq!(
            evaluate_tool("Bash", Some(&serde_json::json!({"command": "git status"}))),
            None
        );
        // Missing/empty command fails open to ALLOW.
        assert_eq!(evaluate_tool("Bash", Some(&serde_json::json!({}))), None);
        assert_eq!(evaluate_tool("Bash", None), None);
    }

    #[test]
    fn evaluate_bash_command_denies_shell_edit_verbs() {
        for cmd in [
            "sed -i s/a/b/ src/lib.rs",
            "awk '{print}' f",
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
        // composed segment (the shell-composition bypass, issue: PR #1985).
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
    fn split_shell_segments_splits_operators() {
        assert_eq!(
            split_shell_segments("a && b || c ; d | e"),
            vec!["a ", " b ", " c ", " d ", " e"]
        );
    }

    #[test]
    fn split_shell_segments_single_command() {
        assert_eq!(split_shell_segments("git status"), vec!["git status"]);
        // Backgrounding `&` is not a split point.
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

    #[test]
    fn build_pretooluse_deny_response_has_expected_shape() {
        let v = build_pretooluse_deny_response("nope");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "nope");
        // Display is compact single-line JSON (protocol: stdout is only the object).
        assert!(!v.to_string().contains('\n'));
    }
}
