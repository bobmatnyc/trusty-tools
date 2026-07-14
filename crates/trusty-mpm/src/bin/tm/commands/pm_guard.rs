//! `tm hook --pm-guard` — PreToolUse enforcement for MPM-managed PM sessions
//! (issue #1977; native sub-agent exemption issue #2014).
//!
//! Why: the deployed PM instructions carry prohibitions P1–P11 ("delegate all
//! file edits to rust-engineer", "never run builds/tests yourself", …) but
//! they are prompt-only — nothing stops the model from calling `Edit`/`Write`
//! or shelling out to `sed`/`make`/`pytest` directly. Claude Code's
//! `PreToolUse` hook is the single mechanical seam that can *block* a tool call
//! before it runs, so wiring a guard here turns those prohibitions from advice
//! into enforcement while still steering the PM toward the Task/Agent tool.
//! Those prohibitions apply only to the PM's OWN direct tool calls — a
//! sub-agent dispatched via the native `Task`/`Agent` tool is doing exactly the
//! delegated work the PM is steered toward, so its Edit/Write calls must be
//! exempt, not denied (issue #2014). The legacy `CLAUDE_MPM_SUB_AGENT` env-var
//! exemption only covers trusty-mpm's own nested-process spawns; it is never
//! set for a native subagent, so those calls were denied, forcing operators to
//! fall back to the all-or-nothing `TRUSTY_MPM_PM_UNRESTRICTED=1` bypass just
//! to get delegated work done.
//! What: [`pm_guard`] is the `tm hook --pm-guard` entry point. It short-circuits
//! (ALLOW) inside sub-agents / disabled-hook shells / the explicit
//! `TRUSTY_MPM_PM_UNRESTRICTED=1` bypass, reads the `PreToolUse` stdin payload,
//! exempts native sub-agent dispatches via [`payload_is_subagent_dispatch`]
//! (the payload's documented `agent_id` field), classifies the tool **locally**
//! (no daemon round-trip — a PreToolUse hook must be fast and must never
//! hard-block when the daemon is down), and either stays silent (ALLOW) or
//! prints a `permissionDecision: "deny"` response (DENY). The classification is
//! a static default-ALLOW + explicit deny-list: [`evaluate_tool`] denies a PM
//! direct edit tool ONLY when it targets a *source-code* file (path-based —
//! issue #2604), denies the forbidden Bash verbs, and allows everything else —
//! including single-file writes to PM-owned orchestration state
//! (`.trusty-mpm/**`, the memory palace, `TASK.md`), docs, and config. Every
//! error path (malformed stdin, missing fields) fails **open** — ALLOW — so a
//! broken hook never wedges the PM. **Opt-in deny-by-default (issue #2231):**
//! the sub-agent exemption is narrowed, ONLY when
//! [`pm_guard_deny_by_default::deny_by_default_enabled`] is truthy, so a
//! delegated dispatch of an [`evaluate_tool`]-guarded tool is denied when
//! [`pm_guard_deny_by_default::persona_status`] resolves to
//! `PersonaStatus::ExplicitlyFailed` (a positively-recorded managed-session
//! persona-deployment failure). This is the ONE place default behavior can
//! change, and only when the operator explicitly opts in — see that module's
//! doc comment for the full #2172-lesson rationale. Unset (the default),
//! Guard 4 is byte-for-byte identical to the pre-#2231 unconditional exemption.
//! Test: `evaluate_tool_*`, `payload_is_subagent_dispatch_*`, and
//! `build_pretooluse_deny_response_*` below cover the tool policy, sub-agent
//! exemption, and JSON shape; the Bash-command classifier (composition and
//! substitution handling) lives in the sibling [`super::pm_guard_bash`] module
//! with its own tests; the opt-in persona gate's policy lives in
//! [`super::pm_guard_deny_by_default`] with its own tests; `tests/tm_hook_pm_guard.rs`
//! exercises the stdin→decision→stdout path (and the env bypasses / sub-agent
//! exemption / fail-open) end to end through the real binary.

use crate::commands::misc::{DISABLE_HOOKS_ENV, SUB_AGENT_ENV, read_stdin_hook_payload};
use crate::commands::pm_guard_bash::evaluate_bash_command;
use crate::commands::pm_guard_deny_by_default::{self, PERSONA_DENY_REASON};

/// Escape-hatch env var: `TRUSTY_MPM_PM_UNRESTRICTED=1` disables all PM
/// enforcement for the invocation.
///
/// Why: the prohibitions exist for the *autonomous* PM loop; when the operator
/// explicitly tells the PM "you do it this time", there must be a way to lift
/// the block without editing settings.json and restarting. Keying it on the
/// exact value `"1"` (rather than mere presence) keeps an accidental empty
/// export from silently disabling enforcement. This is a manual, opt-in,
/// single-named-var override — never set by trusty-mpm's own session-launch or
/// spawn code (verified: no occurrence outside this module and its tests) — so
/// there is nothing for issue #2014 to "retire" in the launcher; the fix there
/// is that [`payload_is_subagent_dispatch`] now makes this bypass unnecessary
/// for ordinary delegated work. It remains available for the rare case an
/// operator wants to lift enforcement on the PM's *own* direct edits too.
/// What: the literal env var name. See [`pm_unrestricted`].
pub(crate) const PM_UNRESTRICTED_ENV: &str = "TRUSTY_MPM_PM_UNRESTRICTED";

/// Single-file mutation tools the PM invokes directly. Whether a given call is
/// denied is decided by its *target path*, not the tool name — see
/// [`evaluate_edit_tool`].
///
/// Why: these are the direct filesystem-mutation tools. Prohibition P1's real
/// target is *source-code implementation* — the PM must hand `.rs`/`.py`/… work
/// to rust-engineer. But a single-file write to PM-owned orchestration state (a
/// session snapshot under `.trusty-mpm/`, the memory palace, a `TASK.md`), a
/// doc, or a config file is legitimate PM work and must NOT be blocked
/// (issue #2604 — Bob's directive: "the PM can write single files").
/// What: matched by exact tool name in [`evaluate_tool`], then classified by
/// target path in [`evaluate_edit_tool`].
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Deny reason for a PM direct write to a *source-code* file (prohibition P1).
const SOURCE_EDIT_REASON: &str = "PM must not implement source code directly (prohibition P1). \
     Delegate the change to rust-engineer via the Task/Agent tool. Single-file writes to \
     non-source files (docs, config, and `.trusty-mpm/` orchestration state) are permitted.";

/// Source-code file extensions a PM direct edit-tool call must not target.
///
/// Why: prohibition P1 blocks the PM from *implementing code* directly; the file
/// extension is the cheap, reliable signal for "this is source the PM should
/// hand to rust-engineer" (issue #2604). Non-code single-file writes (docs,
/// config, data, orchestration state) are legitimate and are NOT listed here.
/// What: lowercase extensions (no dot) matched in [`is_source_code_path`]. The
/// list covers the mainstream compiled/scripting languages; anything not listed
/// is treated as a non-source file and allowed.
/// Test: `is_source_code_path_classifies_by_extension`.
const SOURCE_CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "pyx", "ipynb", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt",
    "kts", "rb", "php", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "swift", "scala", "sc",
    "ex", "exs", "erl", "dart", "sql", "sh", "bash", "zsh", "fish", "lua", "pl", "pm", "r", "mm",
    "vue", "svelte", "proto", "hs", "clj", "cljs", "cljc", "jl", "fs", "fsx", "groovy", "vb",
    "asm", "nim", "zig", "ml", "mli",
];

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
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    // Guard 4: native Claude Code sub-agent dispatch (issue #2014). A
    // Task/Agent-dispatched sub-agent is doing exactly the delegated work the
    // PM is steered toward, so its Edit/Write calls are exempt, not denied —
    // UNLESS the opt-in deny-by-default persona gate (issue #2231) is enabled
    // AND this tool is one `evaluate_tool` would itself guard AND the
    // session's persona is positively confirmed failed. `deny_by_default_enabled`
    // short-circuits the `&&` below to `false` in the (default) unset case, so
    // `evaluate_tool` is never even called on that path — this branch is
    // byte-for-byte identical to the pre-#2231 unconditional exemption when
    // the env var is unset.
    if payload_is_subagent_dispatch(&payload) {
        if pm_guard_deny_by_default::deny_by_default_enabled()
            && evaluate_tool(tool_name, tool_input).is_some()
        {
            let status = pm_guard_deny_by_default::persona_status(url).await;
            if status.should_deny() {
                audit_denied_tool(url, session_id, tool_name, PERSONA_DENY_REASON).await;
                println!("{}", build_pretooluse_deny_response(PERSONA_DENY_REASON));
                return Ok(());
            }
        }
        tracing::debug!(
            tool_name,
            "pm_guard: allow — native sub-agent dispatch (agent_id present in PreToolUse payload)"
        );
        return Ok(());
    }

    let Some(reason) = evaluate_tool(tool_name, tool_input) else {
        // ALLOW: exit 0 with no output so the normal permission flow applies.
        return Ok(());
    };

    // DENY. Record the enforcement action (best-effort, never gating), then
    // emit the block. The audit POST is intentionally the *only* daemon call
    // and fires solely on the rare deny path, so the common ALLOW path adds
    // zero latency to every tool invocation.
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

/// Whether a `PreToolUse` payload originates from a native Claude Code
/// sub-agent (a `Task`/`Agent`-tool-dispatched delegation) rather than the
/// top-level PM's own tool call.
///
/// Why (issue #2014): the PM legitimately dispatches Edit/Write work to
/// rust-engineer (and other) sub-agents via the native `Task`/`Agent` tool.
/// That dispatched sub-agent's tool calls fire their own `PreToolUse` events,
/// but the pre-existing [`SUB_AGENT_ENV`] exemption only covers trusty-mpm's
/// own nested-process spawns (it stamps `CLAUDE_MPM_SUB_AGENT` on processes
/// *it* forks) — a native Task-tool sub-agent never has that var set, so its
/// Edit/Write calls were denied, forcing the all-or-nothing
/// `TRUSTY_MPM_PM_UNRESTRICTED=1` bypass just to unblock ordinary delegation.
/// Claude Code's hooks contract documents `agent_id` as present in the
/// `PreToolUse` JSON payload "only when the hook fires inside a subagent
/// call", specifically so hooks can "distinguish subagent hook calls from
/// main-thread calls" (confirmed against the live hooks reference,
/// <https://code.claude.com/docs/en/hooks>, 2026-07-06) — it is the documented,
/// version-stable signal for exactly this distinction, unlike undocumented
/// process env vars.
/// What: returns `true` when `payload.agent_id` is present and non-empty.
/// `agent_type` is deliberately NOT used as the signal — it is also stamped on
/// a top-level session launched with `--agent`, so alone it would not reliably
/// distinguish a dispatched sub-agent from the PM's own session.
/// Test: `payload_is_subagent_dispatch_true_when_agent_id_present`,
/// `payload_is_subagent_dispatch_false_when_absent_or_empty`; exercised end to
/// end via `pm_guard_native_subagent_dispatch_allows_edit` in
/// `tests/tm_hook_pm_guard.rs`.
fn payload_is_subagent_dispatch(payload: &serde_json::Value) -> bool {
    payload
        .get("agent_id")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// Classify a `PreToolUse` tool call: `Some(reason)` denies, `None` allows.
///
/// Why: the default-ALLOW + explicit deny-list policy. Default-deny would
/// break the PM (it legitimately calls Read/Grep/Task/git-status/mcp tools all
/// the time); only the small, high-confidence set of prohibited actions is
/// blocked.
/// What: routes an [`EDIT_TOOLS`] call through the path-based
/// [`evaluate_edit_tool`] (deny only a source-code write — issue #2604),
/// delegates a `Bash` call to [`evaluate_bash_command`] against its `command`
/// argument, and allows every other tool (Read, Grep, Glob, Task, TodoWrite,
/// WebFetch, `mcp__*`, …).
/// Test: `evaluate_tool_denies_source_code_edits`,
/// `evaluate_tool_allows_pm_orchestration_state`,
/// `evaluate_tool_allows_read_and_task`, `evaluate_tool_delegates_bash`.
pub(crate) fn evaluate_tool(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<&'static str> {
    if EDIT_TOOLS.contains(&tool_name) {
        return evaluate_edit_tool(tool_input);
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

/// Classify a PM direct edit-tool call by its target path (issue #2604).
///
/// Why: prohibition P1 must stop the PM implementing *source code* directly
/// while still letting it write single non-code files — its own orchestration
/// state (`.trusty-mpm/` snapshots, the memory palace), docs, and config. Bob's
/// directive (live dogfooding): "the PM can write single files." The pre-#2604
/// guard blocked *all* Edit/Write, which denied the PM its own session-pause
/// snapshot write to `.trusty-mpm/sessions/session-*.md`.
/// What: reads the target path; ALLOW (`None`) when it is PM-owned orchestration
/// state ([`is_pm_orchestration_path`], the HARD always-allow set — this wins
/// over the source-code rule); DENY ([`SOURCE_EDIT_REASON`]) when it is a
/// source-code file ([`is_source_code_path`]); otherwise ALLOW (a single-file
/// write to a non-source file). A call with no resolvable target path fails
/// open to ALLOW, consistent with the module's fail-open posture.
/// Test: `evaluate_tool_denies_source_code_edits`,
/// `evaluate_tool_allows_pm_orchestration_state`,
/// `evaluate_tool_allows_non_source_single_file_writes`,
/// `evaluate_edit_tool_fails_open_without_path`.
fn evaluate_edit_tool(tool_input: Option<&serde_json::Value>) -> Option<&'static str> {
    let path = edit_tool_target_path(tool_input)?;
    if is_pm_orchestration_path(path) {
        return None;
    }
    if is_source_code_path(path) {
        return Some(SOURCE_EDIT_REASON);
    }
    None
}

/// The filesystem path an edit-tool call targets, if any.
///
/// Why: the edit-tool deny decision (issue #2604) is path-based, so it must read
/// the file the call would write. `Write`/`Edit`/`MultiEdit` name it in
/// `file_path`; `NotebookEdit` uses `notebook_path`.
/// What: returns the first present string of `tool_input.file_path` /
/// `tool_input.notebook_path`, or `None` when neither is present (a malformed
/// call — the caller then fails open).
/// Test: covered via `evaluate_edit_tool_fails_open_without_path` and the
/// `evaluate_tool_*` edit cases.
fn edit_tool_target_path(tool_input: Option<&serde_json::Value>) -> Option<&str> {
    let input = tool_input?;
    input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))
        .and_then(|v| v.as_str())
}

/// Whether `path` is PM-owned orchestration state that must NEVER be blocked.
///
/// Why (issue #2604, HARD requirement): the PM has to persist its own
/// orchestration artifacts — session snapshots and config/override files under
/// `.trusty-mpm/`, its memory palace under the tm-owned `.trusty-tools/` config
/// home, and `TASK.md`-style session files. A session-pause writing
/// `.trusty-mpm/sessions/session-*.md` must always pass, independent of the
/// source-code rule.
/// What: `true` when the basename is `TASK.md` or `MEMORY.md`, or when any path
/// *component* is exactly `.trusty-mpm` or `.trusty-tools`. Component matching
/// (not substring) is deliberate so the crate source dir `crates/trusty-mpm/`
/// — note no leading dot — is NOT matched.
/// Test: `is_pm_orchestration_path_matches_owned_state`.
fn is_pm_orchestration_path(path: &str) -> bool {
    use std::path::{Component, Path};
    let p = Path::new(path);
    if matches!(
        p.file_name().and_then(|s| s.to_str()),
        Some("TASK.md" | "MEMORY.md")
    ) {
        return true;
    }
    p.components().any(|c| {
        matches!(c, Component::Normal(os)
            if matches!(os.to_str(), Some(".trusty-mpm" | ".trusty-tools")))
    })
}

/// Whether `path` names a source-code file (by extension).
///
/// Why: prohibition P1 blocks the PM from *implementing code* directly; the
/// extension is the reliable, cheap signal for "this is source the PM should
/// hand to rust-engineer" (issue #2604).
/// What: `true` when the lowercased file extension is in
/// [`SOURCE_CODE_EXTENSIONS`]. Extension-less files (e.g. `Makefile`, `README`)
/// and non-code extensions (`.md`, `.toml`, `.json`, `.txt`) return `false`.
/// Test: `is_source_code_path_classifies_by_extension`.
fn is_source_code_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| SOURCE_CODE_EXTENSIONS.contains(&e.as_str()))
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
    use crate::commands::pm_guard_bash::SHELL_EDIT_REASON;

    #[test]
    fn evaluate_tool_denies_source_code_edits() {
        // A PM direct edit of a source-code file is still blocked (P1).
        for tool in ["Edit", "Write", "MultiEdit"] {
            assert_eq!(
                evaluate_tool(
                    tool,
                    Some(&serde_json::json!({"file_path": "crates/foo/src/lib.rs"}))
                ),
                Some(SOURCE_EDIT_REASON),
                "expected {tool} on a .rs file to be denied"
            );
        }
        // NotebookEdit names its target in `notebook_path`.
        assert_eq!(
            evaluate_tool(
                "NotebookEdit",
                Some(&serde_json::json!({"notebook_path": "/x/nb.ipynb"}))
            ),
            Some(SOURCE_EDIT_REASON)
        );
    }

    #[test]
    fn evaluate_tool_allows_pm_orchestration_state() {
        // Regression for issue #2604: the exact session-pause snapshot path must
        // be allowed, plus TASK.md and the memory index.
        for path in [
            ".trusty-mpm/sessions/session-20260714-064556.md",
            "TASK.md",
            "/Users/x/.trusty-tools/trusty-mpm/claude-config/projects/p/memory/MEMORY.md",
        ] {
            assert_eq!(
                evaluate_tool("Write", Some(&serde_json::json!({"file_path": path}))),
                None,
                "expected PM-owned state {path} to be allowed"
            );
        }
    }

    #[test]
    fn evaluate_tool_allows_non_source_single_file_writes() {
        // Bob's directive: single-file writes to non-source files are permitted.
        for path in [
            "notes.md",
            "config.json",
            "data.csv",
            "settings.toml",
            "README",
            "out.txt",
        ] {
            assert_eq!(
                evaluate_tool("Write", Some(&serde_json::json!({"file_path": path}))),
                None,
                "expected non-source single-file write {path} to be allowed"
            );
        }
    }

    #[test]
    fn evaluate_edit_tool_fails_open_without_path() {
        // A malformed edit call with no resolvable target path fails open.
        assert_eq!(
            evaluate_tool("Write", Some(&serde_json::json!({"content": "x"}))),
            None
        );
        assert_eq!(evaluate_tool("Edit", None), None);
    }

    #[test]
    fn is_pm_orchestration_path_matches_owned_state() {
        for p in [
            ".trusty-mpm/sessions/session-20260714-064556.md",
            "/abs/path/.trusty-mpm/config.json",
            "/Users/x/.trusty-tools/trusty-mpm/x/memory/MEMORY.md",
            "TASK.md",
            "sub/dir/TASK.md",
        ] {
            assert!(is_pm_orchestration_path(p), "{p} should be PM-owned");
        }
        // The crate source dir `trusty-mpm` (no leading dot) must NOT match.
        for p in ["crates/trusty-mpm/src/lib.rs", "src/main.rs", "notes.md"] {
            assert!(!is_pm_orchestration_path(p), "{p} should NOT be PM-owned");
        }
    }

    #[test]
    fn is_source_code_path_classifies_by_extension() {
        for p in [
            "a.rs",
            "b.py",
            "c.ts",
            "d.tsx",
            "e.go",
            "nb.ipynb",
            "x/y/z.cpp",
        ] {
            assert!(is_source_code_path(p), "{p} should be source");
        }
        for p in [
            "notes.md",
            "Cargo.toml",
            "data.json",
            "TASK.md",
            "README",
            "log.txt",
            ".trusty-mpm/s.md",
        ] {
            assert!(!is_source_code_path(p), "{p} should NOT be source");
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
    fn payload_is_subagent_dispatch_true_when_agent_id_present() {
        assert!(payload_is_subagent_dispatch(&serde_json::json!({
            "agent_id": "agent-abc123",
            "agent_type": "rust-engineer",
            "tool_name": "Edit"
        })));
    }

    #[test]
    fn payload_is_subagent_dispatch_false_when_absent_or_empty() {
        // No agent_id key at all — the PM's own direct tool call.
        assert!(!payload_is_subagent_dispatch(&serde_json::json!({
            "tool_name": "Edit"
        })));
        // Empty-string agent_id must not count as a dispatch signal.
        assert!(!payload_is_subagent_dispatch(&serde_json::json!({
            "agent_id": "",
            "tool_name": "Edit"
        })));
        // `agent_type` alone (e.g. a top-level session launched with
        // `--agent`) must NOT be treated as a sub-agent dispatch.
        assert!(!payload_is_subagent_dispatch(&serde_json::json!({
            "agent_type": "rust-engineer",
            "tool_name": "Edit"
        })));
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
    fn build_pretooluse_deny_response_has_expected_shape() {
        let v = build_pretooluse_deny_response("nope");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "nope");
        // Display is compact single-line JSON (protocol: stdout is only the object).
        assert!(!v.to_string().contains('\n'));
    }
}
