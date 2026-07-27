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
//! What: [`pm_guard`] is the `tm hook --pm-guard` entry point. It resolves the
//! two kill-switch bypasses from the DAEMON (Guards 2/3 — see
//! [`pm_guard_session_flags`] and the "Daemon-held kill-switch flags" note
//! below), short-circuits (ALLOW) inside sub-agents, reads the `PreToolUse`
//! stdin payload, exempts native sub-agent dispatches via
//! [`payload_is_subagent_dispatch`] (the payload's documented `agent_id`
//! field), classifies the tool **locally** (no daemon round-trip for the
//! per-tool classification itself — a PreToolUse hook must stay fast and must
//! never hard-block on tool-classification when the daemon is down; see below
//! for why Guards 2/3 are a deliberate, narrow exception to that rule), and
//! either stays silent (ALLOW) or prints a `permissionDecision: "deny"`
//! response (DENY). The classification is
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
//! **Per-turn file-change budget (issue #2918):** a "file change" deny
//! (source-code Edit/Write/MultiEdit/NotebookEdit, or a shell-based file edit
//! — [`SOURCE_EDIT_REASON`] / [`crate::commands::pm_guard_bash::SHELL_EDIT_REASON`])
//! is no longer an absolute prohibition. [`pm_guard`] now allows up to
//! [`crate::commands::pm_guard_budget::DEFAULT_FILE_CHANGE_BUDGET`] such
//! changes per turn-window (see that module for the turn-window heuristic)
//! before hard-blocking with a budget-exhausted message that names the count
//! and, via [`crate::commands::pm_guard_routing::delegation_hint_for_path`],
//! the specialist matching the target file's content type instead of a
//! hardcoded `rust-engineer`.
//! Build/test and network denials are NOT budget-eligible and stay absolute.
//! **Worktree-tmp guard (issue #3977, follow-up to #3955):** [`pm_guard`] calls
//! [`crate::commands::pm_guard_bash::evaluate_worktree_add_command`] DIRECTLY,
//! before Guard 4's early return, denying any `git worktree add` whose target
//! resolves under `/tmp`, `/private/tmp`, `/var/folders`, `$TMPDIR`, or the
//! harness scratchpad. It also pierces Guard 1 (`CLAUDE_MPM_SUB_AGENT`) —
//! Guards 2/3 (below) run first and are UNAFFECTED, since Guard 1/4 are
//! automatic markers a spawn helper stamps on every nested/dispatched
//! subagent process (no human decides per-invocation whether they're set),
//! unlike Guards 2/3 — see [`pm_guard`]'s body for the exact reordering this
//! required (Guard 1's check moved after the stdin payload read, specifically
//! so this worktree check — which needs the payload — can run first). This
//! guard is a different threat model from Guards 2/3 entirely and is left
//! exactly as-is by issue #3981 — do not fold it into that redesign.
//! **Daemon-held kill-switch flags (issue #3981, replacing an env-var design
//! that predated this crate):** Guards 2/3 (`disable_hooks`/`pm_unrestricted`)
//! used to read `TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED`
//! directly from LIVE process env on every `PreToolUse` call. Claude Code
//! re-spawns this hook subprocess fresh each time and re-injects
//! `.claude/settings.json`'s top-level `env` object into it (live-reloaded
//! mid-session) — a `.json` file is not in [`SOURCE_CODE_EXTENSIONS`], so a
//! plain PM `Write`/`Edit` to that file fell through [`evaluate_edit_tool`]
//! to an unconditional ALLOW, letting the PM (or any Guard-4-exempt
//! subagent) self-exempt from the ENTIRE guard with one file write, no
//! restart, no observable disruption. [`pm_guard_session_flags::guard_flags`]
//! replaces both env reads with a daemon round trip resolving the flags from
//! the session's OWN record — captured ONCE at `tm sessions new`/`start`/
//! `resume` time from the operator's launching shell
//! (`session_manager::guard_flags`, `core::pm_guard_launch_env`), and never
//! written by this hook process. Disabling the guard now requires an actual
//! session restart with the flag set — an operator-initiated act, matching
//! `core::project_trust`'s consult-once-at-launch precedent, NOT a
//! file-permission boundary. FAILS TOWARD GUARD-ACTIVE: an unreachable
//! daemon, an unresolvable session, or an unconfirmed tmux pane (issue
//! #3600's lesson, applied here) all resolve to the flags staying `false`/
//! `false` — never a bypass; see that module's doc for the full design and
//! its scope note on local (non-managed) sessions.
//! Test: `evaluate_tool_*`, `payload_is_subagent_dispatch_*`, and
//! `build_pretooluse_deny_response_*` below cover the tool policy, sub-agent
//! exemption, and JSON shape; the Bash-command classifier (composition and
//! substitution handling) lives in the sibling [`super::pm_guard_bash`] module
//! with its own tests; the opt-in persona gate's policy lives in
//! [`super::pm_guard_deny_by_default`] with its own tests; the daemon-held
//! kill-switch resolution lives in [`super::pm_guard_session_flags`] with its
//! own tests; `tests/tm_hook_pm_guard.rs` exercises the stdin→decision→stdout
//! path (and the sub-agent exemption / fail-open) end to end through the real
//! binary.

use std::path::PathBuf;

use crate::commands::misc::{SUB_AGENT_ENV, read_stdin_hook_payload};
use crate::commands::pm_guard_bash::{
    SHELL_EDIT_REASON, evaluate_bash_command, evaluate_worktree_add_command,
    extract_shell_edit_target,
};
use crate::commands::pm_guard_budget::{self, BudgetDecision, DEFAULT_FILE_CHANGE_BUDGET};
use crate::commands::pm_guard_deny_by_default::{self, PERSONA_DENY_REASON};
use crate::commands::pm_guard_routing::{GENERIC_ENGINEER_HINT, delegation_hint_for_path};
use crate::commands::pm_guard_session_flags;

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
///
/// Why (issue #2918): this text is the pure classifier's internal deny
/// signal — `pm_guard` compares a returned reason against this constant to
/// decide budget-eligibility, then (once the per-turn budget is exhausted)
/// builds a fresh, content-routed message rather than printing this one
/// verbatim. It is retained (rather than inlined) so `evaluate_tool`/
/// `evaluate_edit_tool` stay pure and their existing unit tests need no
/// budget/routing awareness.
const SOURCE_EDIT_REASON: &str = "PM must not implement source code directly (prohibition P1). \
     Delegate the change to rust-engineer via the Task/Agent tool. Single-file writes to \
     non-source files (docs, config, and `.trusty-mpm/` orchestration state) are permitted.";

/// Deny reason for a PM direct write to a guard-owned enforcement-state file.
///
/// Why (issue #3981 Part 3): distinct from [`SOURCE_EDIT_REASON`] so `pm_guard`'s
/// budget gate (which only recognizes that exact reason) never treats this as
/// budget-eligible — forging the guard's own inputs is an absolute deny, not a
/// routine file edit.
const GUARD_STATE_EDIT_REASON: &str = "PM must not write pm_guard's own enforcement state \
     directly (issue #3981) — this file is an INPUT to the guard, not PM orchestration \
     output. If the turn budget or project-trust decision needs to change, that is an \
     operator action, not something the PM edits for itself.";

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
/// The two kill-switch bypasses (`disable_hooks`/`pm_unrestricted`) are
/// resolved from the daemon's held session record via
/// [`pm_guard_session_flags::guard_flags`] and checked FIRST, before the
/// stdin payload is even read — genuine operator-set escape hatches (see
/// that module's doc for the #3981 redesign), lifting EVERY check below,
/// including the absolute worktree-tmp guard, when set. Nested MPM
/// sub-agents (`CLAUDE_MPM_SUB_AGENT`) are different: that marker is stamped
/// automatically by a spawn helper, not operator-decided per call, so it is
/// checked LATER — after the worktree-tmp guard has already had a chance to
/// fire (issue #3977; PR #3978 round 2) — and exempts everything else,
/// matching the pre-#3977 behavior. Any missing/malformed input degrades to
/// ALLOW rather than wedging the PM. The per-tool CLASSIFICATION below
/// (`evaluate_tool` and friends) is still computed purely, with no daemon
/// call, so a down daemon can never hard-block a tool call on that path —
/// only the kill-switch resolution above makes a (tightly-timed, fail-toward-
/// active) daemon round trip.
/// What: returns `Ok(())` (ALLOW, no stdout) for every short-circuit, a payload
/// with no `tool_name`, or an [`evaluate_tool`] verdict of ALLOW. On DENY it
/// fires a best-effort audit POST to `<url>/hooks` (tight timeout, result
/// ignored — audit only, never gating) and prints the
/// `hookSpecificOutput.permissionDecision = "deny"` JSON to stdout, then
/// returns. `url` is used only for the audit POST.
/// Test: sub-agent short-circuit + fail-open + deny/allow are covered end to
/// end in `tests/tm_hook_pm_guard.rs`; the pure policy is covered by this
/// module's unit tests; the kill-switch daemon round trip is covered by
/// [`pm_guard_session_flags`]'s own tests.
pub(crate) async fn pm_guard(url: &str) -> anyhow::Result<()> {
    // Guards 2/3: the two kill-switch bypasses, resolved from the DAEMON's
    // held session record rather than live process env (issue #3981 Part 2).
    // Checked FIRST (ahead of Guard 1), mirroring this function's original
    // ordering. Formerly these read `TRUSTY_MPM_DISABLE_HOOKS`/
    // `TRUSTY_MPM_PM_UNRESTRICTED` directly from `std::env::var` — but Claude
    // Code re-spawns this hook subprocess fresh on EVERY `PreToolUse` call
    // and re-injects `.claude/settings.json`'s `env` block into it, so a
    // single PM-writable settings.json edit disabled the ENTIRE guard,
    // persistently, with no restart and no observable session disruption.
    // `pm_guard_session_flags::guard_flags` instead resolves these from the
    // session's own daemon record, captured ONCE at `tm sessions new`/
    // `start`/`resume` time from the operator's launching shell
    // (`session_manager::guard_flags`) — this hook process never writes that
    // record, only reads it, so there is no live flip: disabling the guard
    // now requires an actual session restart with the flag set (Bob's
    // decision — "resume-with-flag is fine", matching
    // `core::project_trust`'s consult-once-at-launch precedent).
    // DELIBERATE BEHAVIOR CHANGE: this makes Guards 2/3 a daemon round trip
    // on every `PreToolUse` call (tight 500ms-connect/2s-total budget,
    // mirroring `pm_guard_deny_by_default::persona_status`'s), and an
    // unreachable/slow daemon now resolves to GUARD FULLY ACTIVE — the
    // opposite of this module's usual fail-OPEN posture. That inversion is
    // intentional: fail-open exists so a down daemon never blocks a tool
    // call for the ordinary classification path below; a KILL-SWITCH query
    // must never fail toward "disabled" or crashing/pausing the daemon
    // becomes the new universal bypass. See
    // `pm_guard_session_flags`'s module doc for the full design, including
    // its scope note on local (non-managed) sessions.
    let flags = pm_guard_session_flags::guard_flags(url).await;
    if flags.disable_hooks || flags.pm_unrestricted {
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

    // ABSOLUTE guard (issue #3977, worktree-hygiene follow-up to #3955) —
    // deliberately placed BEFORE Guard 1's and Guard 4's subagent-exemption
    // early returns, and called DIRECTLY rather than routed through
    // `evaluate_tool`. Two prior investigations (PR #3978 rounds 1 and 2)
    // established that BOTH exemptions must be pierced, for the same
    // underlying reason: `evaluate_tool` (and therefore
    // `pm_guard_bash::evaluate_bash_command`, its only Bash-classifying
    // callee) is NEVER reached for a payload carrying a non-empty `agent_id`
    // (Guard 4 returns first) NOR for a process with `CLAUDE_MPM_SUB_AGENT`
    // set (Guard 1, formerly the very first check in this function, returned
    // before the stdin payload was even read) — so a `git worktree add` rule
    // placed inside the ordinary Bash classifier, or checked only after
    // either exemption, would be a silent no-op for the calling patterns
    // (native Task/Agent dispatch AND trusty-agents' own subprocess-spawned
    // subagents — `crates/trusty-agents/src/subprocess/spawn.rs:189-192`,
    // `crates/trusty-agents/src/agents/claude_code_runner/run.rs:211-215`)
    // that actually provision these worktrees in practice. Guards 2/3 above
    // are DELIBERATELY still checked earlier (before this) — they are human
    // escape hatches, not automatic markers; see their comments. DO NOT move
    // this block after Guard 1 or Guard 4, and do NOT fold it into
    // `evaluate_tool`/`evaluate_bash_command` — doing so re-introduces the
    // no-op this comment exists to prevent.
    if tool_name == "Bash" {
        let command = tool_input
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let hook_cwd = payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        if let Some(reason) = evaluate_worktree_add_command(command, &hook_cwd) {
            audit_denied_tool(url, session_id, tool_name, reason).await;
            println!("{}", build_pretooluse_deny_response(reason));
            return Ok(());
        }
    }

    // Guard 1: never block a nested MPM sub-agent for anything else — it is
    // doing the delegated work the PM is being steered toward. Moved here
    // (originally the first line of this function, short-circuiting before
    // any stdin read) specifically so the ABSOLUTE worktree guard above —
    // which needs the payload — runs first and cannot be silently bypassed
    // by this automatic marker. See that guard's comment for the full
    // rationale; see Guards 2/3 above for why THOSE stayed in their original
    // position instead.
    if std::env::var_os(SUB_AGENT_ENV).is_some() {
        return Ok(());
    }

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

    // Budget gate (issue #2918): a file-change deny (source-code Edit/Write or
    // a shell-based file edit) is no longer an absolute prohibition — it is
    // allowed up to DEFAULT_FILE_CHANGE_BUDGET times per turn-window before
    // hard-blocking. Build/test and network denials are NOT budget-eligible
    // and fall through to the unconditional deny below unchanged.
    if reason == SOURCE_EDIT_REASON || reason == SHELL_EDIT_REASON {
        let target_path = if EDIT_TOOLS.contains(&tool_name) {
            edit_tool_target_path(tool_input).map(str::to_owned)
        } else {
            let command = tool_input
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            extract_shell_edit_target(command)
        };
        let hint = target_path
            .as_deref()
            .map(delegation_hint_for_path)
            .unwrap_or(GENERIC_ENGINEER_HINT);

        return match pm_guard_budget::record_file_change(session_id, DEFAULT_FILE_CHANGE_BUDGET) {
            BudgetDecision::Allowed { .. } => {
                // Within budget: ALLOW, exactly like any other allowed call —
                // no stdout, no audit POST (keeps the common path fast).
                Ok(())
            }
            BudgetDecision::Exhausted { budget } => {
                let exhausted_reason = format!(
                    "PM file-change budget {budget}/{budget} used this turn (prohibitions P1/P5). \
                     Delegate further changes to {hint} via the Task/Agent tool."
                );
                audit_denied_tool(url, session_id, tool_name, &exhausted_reason).await;
                println!("{}", build_pretooluse_deny_response(&exhausted_reason));
                Ok(())
            }
        };
    }

    // DENY (non-budget-eligible: build/test, network, or the opt-in persona
    // gate). Record the enforcement action (best-effort, never gating), then
    // emit the block. The audit POST is intentionally the *only* daemon call
    // and fires solely on the rare deny path, so the common ALLOW path adds
    // zero latency to every tool invocation.
    audit_denied_tool(url, session_id, tool_name, reason).await;
    println!("{}", build_pretooluse_deny_response(reason));
    Ok(())
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
    // Checked BEFORE the orchestration allow (issue #3981 Part 3): the guard's
    // own enforcement-state files live UNDER `.trusty-mpm`/`.trusty-tools`, so
    // without this they would fall straight through `is_pm_orchestration_path`'s
    // component match into an unconditional ALLOW — the PM could edit its own
    // turn-budget counter or the project-trust store via a plain Edit/Write.
    if is_guard_state_path(path) {
        return Some(GUARD_STATE_EDIT_REASON);
    }
    if is_pm_orchestration_path(path) {
        return None;
    }
    if is_source_code_path(path) {
        return Some(SOURCE_EDIT_REASON);
    }
    None
}

/// Whether `path` is one of pm_guard's OWN enforcement-state files — a
/// denylist-within-the-allowlist (issue #3981 Part 3).
///
/// Why: `is_pm_orchestration_path` deliberately allows any path under
/// `.trusty-mpm`/`.trusty-tools` (issue #2604) so the PM can write its own
/// session/config state — but two files under those same roots are INPUTS to
/// this guard, not PM-owned output: `pm_guard_budget`'s per-session turn
/// counter (`<root>/state/pm_guard_turn_budget/<session>.json`) and
/// `core::project_trust`'s store (`<root>/project-trust.json`). Letting the PM
/// edit either would let it forge the very decisions this guard makes about it.
/// What: `true` when the basename is exactly `project-trust.json`, or when any
/// path component is exactly `pm_guard_turn_budget`. Component/basename
/// matching (not substring) mirrors [`is_pm_orchestration_path`]'s discipline.
/// Test: `is_guard_state_path_matches_enforcement_state`,
/// `evaluate_tool_denies_guard_state_writes`.
fn is_guard_state_path(path: &str) -> bool {
    use std::path::{Component, Path};
    let p = Path::new(path);
    if p.file_name().and_then(|s| s.to_str()) == Some("project-trust.json") {
        return true;
    }
    p.components()
        .any(|c| matches!(c, Component::Normal(os) if os.to_str() == Some("pm_guard_turn_budget")))
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
    fn is_guard_state_path_matches_enforcement_state() {
        for p in [
            "/Users/x/.trusty-tools/trusty-mpm/project-trust.json",
            "project-trust.json",
            "/Users/x/.trusty-mpm/state/pm_guard_turn_budget/sess-abc.json",
            ".trusty-mpm/state/pm_guard_turn_budget/_default.json",
        ] {
            assert!(is_guard_state_path(p), "{p} should be guard-owned state");
        }
        for p in [
            ".trusty-mpm/sessions/session-20260714-064556.md",
            "TASK.md",
            "/Users/x/.trusty-tools/trusty-mpm/claude-config/projects/p/memory/MEMORY.md",
            "notes.md",
        ] {
            assert!(
                !is_guard_state_path(p),
                "{p} should NOT be guard-owned state"
            );
        }
    }

    #[test]
    fn evaluate_tool_denies_guard_state_writes() {
        for path in [
            "/Users/x/.trusty-tools/trusty-mpm/project-trust.json",
            ".trusty-mpm/state/pm_guard_turn_budget/sess-abc.json",
        ] {
            assert_eq!(
                evaluate_tool("Write", Some(&serde_json::json!({"file_path": path}))),
                Some(GUARD_STATE_EDIT_REASON),
                "expected {path} to be denied"
            );
        }
        // The ordinary #2604 orchestration-write case must still be allowed.
        assert_eq!(
            evaluate_tool(
                "Write",
                Some(&serde_json::json!({"file_path": ".trusty-mpm/sessions/session-1.md"}))
            ),
            None
        );
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
