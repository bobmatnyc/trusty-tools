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
//! Guard 2 (`TRUSTY_MPM_DISABLE_HOOKS`) and Guard 3
//! (`TRUSTY_MPM_PM_UNRESTRICTED`) run first and are UNAFFECTED, since a
//! round 2 code-critic review (PR #3978) established that Guards 1/4 are
//! automatic markers a spawn helper stamps on every nested/dispatched
//! subagent process (no human decides per-invocation whether they're set),
//! while nothing in this codebase's Rust source ever sets
//! `TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED` programmatically via
//! `std::env::var` — see [`pm_guard`]'s body for the exact reordering this
//! required (Guard 1's check moved after the stdin payload read, specifically
//! so this worktree check — which needs the payload — can run first). **This
//! is NOT the same as "operator-only"**: Claude Code's `settings.json`
//! supports a top-level `env` object applied to every session/subprocess it
//! spawns (hooks included, live-reloaded mid-session), and a `Write`/`Edit`
//! to `.claude/settings.json` is not itself blocked by this guard (`.json` is
//! not in [`SOURCE_CODE_EXTENSIONS`], so [`evaluate_edit_tool`] falls through
//! to an unconditional ALLOW) — so the PM, or any Guard-4-exempt subagent,
//! can set either var there and self-exempt from this entire guard. That gap
//! predates this PR by many issues and is NOT introduced or widened by it;
//! tracked separately as issue #3981, not fixed here. It was the first place
//! in the file to pierce an automatic-marker exemption; see that call
//! site's comment for why it must never be routed through [`evaluate_tool`]
//! instead.
//! **Main-checkout destructive-git guard (ADR-0037):** immediately after the
//! worktree-tmp guard, and ahead of the same two exemptions,
//! [`crate::commands::pm_guard_bash::evaluate_main_checkout_destructive_command`]
//! denies `git checkout -- <pathspec>` / `git restore` / `git reset --hard` /
//! `git clean -f…` and their siblings when the directory they would act on is
//! a project's MAIN CHECKOUT rather than a worktree. ADR-0037's
//! write-restriction amendment states that rule and records its enforcement as
//! "mechanical and NOT YET BUILT"; this is that mechanism for the destructive
//! half. It pierces the subagent exemptions because the incident behind it was
//! an AGENT dispatched by a different session — read-only git,
//! `checkout -b`, a plain `git reset`, and everything under
//! `.claude/worktrees/**` stay allowed. No daemon is consulted, so there is no
//! unreachable-daemon arm to fail open through; see that module's doc for the
//! registry it deliberately does not gate on and why.
//! **Main-checkout write boundary (ADR-0044, enforced by ADR-0048):** the
//! destructive-git rule above covers only whole-tree destruction, so an
//! ordinary `Write` to a `.rs` file in a shared checkout — the write the
//! reported incident was made of — passed every check in this file. Two rules
//! close it, both placed with the destructive rule and ahead of the same two
//! exemptions, because ADR-0044 binds the PM AND every agent it dispatches:
//! [`crate::commands::pm_guard_bash::evaluate_main_checkout_commit_command`]
//! denies `git commit` in a main checkout, and
//! [`crate::commands::pm_guard_write_boundary::evaluate_main_checkout_write`]
//! denies an edit tool whose target is a SOURCE file there. Documents,
//! configuration, and everything in a worktree stay writable — that is
//! ADR-0044's boundary, and the source-vs-not question is answered by the same
//! [`SOURCE_CODE_EXTENSIONS`] the PM rule uses, so the two cannot drift apart.
//! **Worktree grant for dispatched writers (ADR-0048):** enforcement alone
//! would block all work, because a dispatched writer in a main checkout had
//! nowhere else to write —
//! [`trusty_mpm::project::worktree_enabled_for_project`] never had a production
//! caller. [`crate::commands::pm_guard_worktree_grant::evaluate_worktree_grant`]
//! gives it somewhere: on an `Agent` dispatch made from a main checkout by an
//! agent that is not positively known to be read-only, it emits
//! `hookSpecificOutput.updatedInput` carrying the dispatch's own input with
//! `isolation: "worktree"` added, so the harness provisions the worktree under
//! `.claude/worktrees/` (ADR-0036) and reclaims it on completion. Trusty-mpm
//! creates nothing, per ADR-0044 decision 4. It runs after the fan-out denial
//! (so only the PM reaches it) and BEFORE the #4480 concurrency check, which
//! must not deny a dispatch that is about to stop sharing the tree. Unlike its
//! neighbours it does NOT fail open on an unknown agent — see that module's
//! doc for why the fail-open direction stops at the main checkout.
//! **Subagent fan-out denial (issue #4784):** [`pm_guard`] calls
//! [`crate::commands::pm_guard_fanout::evaluate_subagent_fanout`] DIRECTLY,
//! before Guards 1 and 4, denying `Task`/`Agent` when the calling session is
//! itself a subagent — the PM keeps dispatching, `SendMessage` is never
//! denied, and an indeterminate caller context fails OPEN. It sits ahead of
//! both exemptions for the same reason the worktree guard does: both return
//! ALLOW exactly when the caller IS a subagent, the only case the rule fires
//! on. See that module's doc for the two markers it trusts and why.
//! **Per-subagent context ceiling (issue #4837):** immediately after the
//! fan-out check — and ahead of the same two exemptions, for the same reason —
//! [`pm_guard`] measures the calling subagent's accumulated context via
//! [`crate::commands::pm_guard_cost::evaluate_agent_cost`] and denies its next
//! tool call once that context crosses `agent_cost.max_tokens`. The PM is
//! never evaluated (the whole block is gated on `caller_is_subagent`) and
//! every failure to measure fails OPEN. Two properties keep the stop from
//! being worse than the overrun: the hard stop ships OFF (`max_tokens = 0`,
//! opt-in) and, when an operator enables it,
//! [`crate::commands::pm_guard_cost::is_persistence_escape`] keeps
//! `SendMessage` and the git commit/push commands permitted so a stopped agent
//! never loses work. The warning level ALLOWS and reaches the agent once, as
//! `additionalContext` — see [`build_pretooluse_context_response`]. See
//! [`trusty_mpm::core::agent_cost`] for the cost model and the thresholds'
//! derivation from the measured transcript distribution.
//! Test: `evaluate_tool_*`, `payload_is_subagent_dispatch_*`, and
//! `build_pretooluse_deny_response_*` below cover the tool policy, sub-agent
//! exemption, and JSON shape; the Bash-command classifier (composition and
//! substitution handling) lives in the sibling [`super::pm_guard_bash`] module
//! with its own tests; the opt-in persona gate's policy lives in
//! [`super::pm_guard_deny_by_default`] with its own tests; `tests/tm_hook_pm_guard.rs`
//! exercises the stdin→decision→stdout path (and the env bypasses / sub-agent
//! exemption / fail-open) end to end through the real binary.

use std::path::PathBuf;

use crate::commands::misc::{DISABLE_HOOKS_ENV, SUB_AGENT_ENV, read_stdin_hook_payload};
use crate::commands::pm_guard_bash::{
    SHELL_EDIT_REASON, evaluate_bash_command, evaluate_main_checkout_commit_command,
    evaluate_main_checkout_destructive_command, evaluate_worktree_add_command,
    extract_shell_edit_target,
};
use crate::commands::pm_guard_budget::{self, BudgetDecision, DEFAULT_FILE_CHANGE_BUDGET};
use crate::commands::pm_guard_cost;
use crate::commands::pm_guard_deny_by_default::{self, PERSONA_DENY_REASON};
use crate::commands::pm_guard_dispatch;
use crate::commands::pm_guard_fanout;
use crate::commands::pm_guard_routing::{GENERIC_ENGINEER_HINT, delegation_hint_for_path};
use crate::commands::pm_guard_worktree_grant;
use crate::commands::pm_guard_write_boundary;
use trusty_mpm::core::agent_cost::{self, AgentCostConfig, BudgetStatus};
use trusty_mpm::core::config::MpmConfig;

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
pub(crate) const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

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
/// It must be fast and fail-open: CI/build shells (`TRUSTY_MPM_DISABLE_HOOKS`)
/// and the explicit `TRUSTY_MPM_PM_UNRESTRICTED=1` operator bypass short-circuit
/// to ALLOW before the stdin payload is even read — both are genuine
/// human-operated escape hatches (nothing in this codebase sets either
/// programmatically), so they lift EVERY check below, including the absolute
/// worktree-tmp guard. Nested MPM sub-agents (`CLAUDE_MPM_SUB_AGENT`) are
/// different: that marker is stamped automatically by a spawn helper, not
/// operator-decided per call, so it is checked LATER — after the worktree-tmp
/// guard has already had a chance to fire (issue #3977; PR #3978 round 2) —
/// and exempts everything else, matching the pre-#3977 behavior. Any missing/
/// malformed input degrades to ALLOW rather than wedging the PM. The decision
/// is computed purely from the static tool classification — never a daemon
/// call — so a down daemon can never hard-block a tool call.
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
    // Guard 2: universal opt-out for CI / build shells that can't edit
    // settings.json without a restart. Checked FIRST (ahead of Guard 1, a
    // reordering from this function's original shape — see the code-critic
    // round 2 finding on PR #3978): this and Guard 3 below are never SET
    // programmatically anywhere in this codebase's Rust source (verified: no
    // `std::env::var`/`set_var` occurrence outside doc comments and this
    // module) — so they must keep working exactly as before, including
    // against the ABSOLUTE worktree-tmp guard below: whoever sets
    // `TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED` gets exactly
    // that, no exceptions. That is a narrower claim than "operator-only",
    // though: Claude Code's `settings.json` `env` object can set either var
    // for every hook invocation, and a write to `.claude/settings.json` is
    // not itself blocked by this guard (see the module doc above, "This is
    // NOT the same as 'operator-only'") — so the PM or an exempt subagent CAN
    // self-exempt via that path. That gap predates this PR by many issues and
    // is out of scope here; tracked as issue #3981. Guard 1 (`SUB_AGENT_ENV`),
    // by contrast, is an
    // AUTOMATIC marker a spawn helper stamps on every nested MPM subagent
    // process with no per-invocation env-write decision involved — see
    // Guard 1's own comment below for why it is checked LATER, after the
    // worktree guard, instead of here.
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
    // Hoisted out of the Bash block below (ADR-0048): the main-checkout write
    // boundary and the worktree grant both need the same directory, and
    // resolving it twice would be the way the three rules drift into
    // disagreeing about which tree the call is standing in.
    let hook_cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    if tool_name == "Bash" {
        let command = tool_input
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if let Some(reason) = evaluate_worktree_add_command(command, &hook_cwd) {
            audit_denied_tool(url, session_id, tool_name, reason).await;
            println!("{}", build_pretooluse_deny_response(reason));
            return Ok(());
        }
        // ABSOLUTE guard (ADR-0037) — the same placement, and for the same
        // structural reason, as the worktree-tmp guard directly above: the
        // incident it exists for was an AGENT dispatched by another session,
        // so a rule that ran after Guard 1 or Guard 4 would be a no-op for
        // exactly the calling pattern it must catch. See
        // `pm_guard_bash::main_checkout` for the scope this piercing is drawn
        // to (irreversible destruction of another session's uncommitted work,
        // nothing wider) and for why no daemon is consulted.
        if let Some(reason) = evaluate_main_checkout_destructive_command(command, &hook_cwd) {
            audit_denied_tool(url, session_id, tool_name, &reason).await;
            println!("{}", build_pretooluse_deny_response(&reason));
            return Ok(());
        }
        // ABSOLUTE guard (ADR-0044 decision 1, enforced by ADR-0048) — the
        // same placement and the same reason as the two above. A commit is
        // where a write becomes permanent on a branch other sessions are
        // standing on, and the reported incident is a commit landing on
        // another workstream's branch. See `pm_guard_bash::main_checkout` for
        // why branch-MOVING verbs are deliberately not folded in here.
        if let Some(reason) = evaluate_main_checkout_commit_command(command, &hook_cwd) {
            audit_denied_tool(url, session_id, tool_name, &reason).await;
            println!("{}", build_pretooluse_deny_response(&reason));
            return Ok(());
        }
    }

    // ABSOLUTE guard (ADR-0044 decision 1, enforced by ADR-0048) — the write
    // boundary for edit tools, placed with the Bash rules above and ahead of
    // Guards 1 and 4 for the identical structural reason: ADR-0044 binds "the
    // PM and every agent it dispatches", and both exemptions early-return
    // ALLOW precisely for the dispatched agents this rule exists to bind. It
    // is NOT routed through `evaluate_tool` — that path asks who is writing
    // and is budgeted and subagent-exempt, while this one asks where the write
    // lands and holds for everyone. DO NOT move it below either exemption.
    if let Some(reason) =
        pm_guard_write_boundary::evaluate_main_checkout_write(tool_name, tool_input, &hook_cwd)
    {
        audit_denied_tool(url, session_id, tool_name, &reason).await;
        println!("{}", build_pretooluse_deny_response(&reason));
        return Ok(());
    }

    // #4784: a subagent must never dispatch further subagents. Placed here,
    // before Guards 1 and 4, for the same structural reason as the worktree
    // guard above: both exemptions early-return ALLOW precisely when the
    // caller IS a subagent, which is the only case this rule fires on — so a
    // fan-out check placed after either one, or folded into `evaluate_tool`,
    // would be a guaranteed no-op. Guards 2/3 stay ahead of it (they are
    // human escape hatches, not automatic markers — see their comments).
    // It fails OPEN: `caller_is_subagent` reports false whenever neither the
    // payload's `agent_id` nor `CLAUDE_MPM_SUB_AGENT` is present, so an
    // unrecognised context allows the dispatch rather than blocking the PM.
    let caller_is_subagent = pm_guard_fanout::caller_is_subagent(&payload);

    if let Some(reason) = pm_guard_fanout::evaluate_subagent_fanout(tool_name, caller_is_subagent) {
        audit_denied_tool(url, session_id, tool_name, reason).await;
        println!("{}", build_pretooluse_deny_response(reason));
        return Ok(());
    }

    // ADR-0048 Part A: a dispatched writer standing in a main checkout is GIVEN
    // a worktree rather than refused one. Placed after the fan-out deny (so a
    // subagent's dispatch is already blocked and only the PM reaches here) and
    // BEFORE the #4480 concurrency check below, which must not run at all on a
    // dispatch that is about to be isolated — the rewritten dispatch no longer
    // shares this tree, so denying it for a sibling in this tree would be a
    // false deny, and a `PreToolUse` hook may print exactly one object anyway.
    //
    // The daemon's own `matcher: "*"` tracker hook still records this dispatch
    // from the ORIGINAL payload, so its delegation reads as unisolated. That
    // record is inert: every dispatch from this main checkout takes this branch
    // and returns before querying, so nothing ever reads it to deny with.
    if !caller_is_subagent
        && let Some(grant) =
            pm_guard_worktree_grant::evaluate_worktree_grant(tool_name, tool_input, &hook_cwd)
    {
        match grant {
            pm_guard_worktree_grant::WorktreeGrant::Rewrite(updated_input) => {
                println!(
                    "{}",
                    pm_guard_worktree_grant::build_worktree_grant_response(&updated_input)
                );
            }
            pm_guard_worktree_grant::WorktreeGrant::Deny(reason) => {
                audit_denied_tool(url, session_id, tool_name, reason).await;
                println!("{}", build_pretooluse_deny_response(reason));
            }
        }
        return Ok(());
    }

    // #4480: the PM must not put a SECOND file-mutating subagent into a working
    // directory that already has one — they race over a single git HEAD and git
    // does not refuse the collision. Placed here, after the fan-out guard,
    // because it fires only on the PM's own dispatch: Guards 1 and 4 below
    // early-return ALLOW precisely when the caller IS a subagent, and such a
    // caller's dispatch has already been denied above. The `caller_is_subagent`
    // gate keeps a subagent from paying for a check that can never apply to it.
    // Fails OPEN throughout (see `pm_guard_dispatch`): a read-only or isolated
    // dispatch never reaches the daemon at all, and a down daemon answers
    // "nobody else is here".
    if !caller_is_subagent
        && let Some(reason) =
            pm_guard_dispatch::evaluate(url, &payload, tool_name, tool_input, session_id).await
    {
        audit_denied_tool(url, session_id, tool_name, &reason).await;
        println!("{}", build_pretooluse_deny_response(&reason));
        return Ok(());
    }

    // Text of a pending agent-cost notice, emitted at whichever subagent ALLOW
    // exit this call reaches (Guard 1 or Guard 4). Deferred rather than printed
    // where it is computed because a `PreToolUse` hook's stdout may carry
    // exactly one object: printing it below and then falling through to Guard
    // 4's persona gate could emit a second one. See `emit_cost_notice`.
    let mut cost_notice: Option<String> = None;

    // #4837: per-subagent context ceiling. Placed here for the same structural
    // reason as the fan-out guard directly above — Guards 1 and 4 below
    // early-return ALLOW precisely when the caller IS a subagent, which is the
    // only case this rule fires on, so a check placed after either one would be
    // a guaranteed no-op. Gated on `caller_is_subagent` so the PM never pays
    // the config load or the transcript read, and can never be stopped by it.
    // Fails OPEN throughout (see `core::agent_cost`): an unreadable transcript,
    // a missing usage record, or a disabled config all reach `Ok`.
    if caller_is_subagent {
        let cost_config = MpmConfig::load_default().agent_cost;
        let (status, tokens) = pm_guard_cost::evaluate_agent_cost(&payload, &cost_config).await;
        match status {
            // The stop keeps a narrow allowlist open so a stopped agent can
            // still save and report what it has — see
            // `pm_guard_cost::is_persistence_escape`. Without it the deny told
            // the agent to report back through a channel the same deny closed.
            BudgetStatus::Exceeded
                if !pm_guard_cost::is_persistence_escape(tool_name, tool_input) =>
            {
                let reason = agent_cost::stop_reason(tokens, cost_config.max_tokens);
                audit_denied_tool(url, session_id, tool_name, &reason).await;
                println!("{}", build_pretooluse_deny_response(&reason));
                return Ok(());
            }
            BudgetStatus::Warning => {
                // ALLOW, and tell the AGENT — once, at the exit below.
                // `additionalContext` is the field that makes that possible
                // without touching the decision: Claude Code injects it next to
                // the tool result, and because the object carries no
                // `permissionDecision` the call still falls through the normal
                // permission flow ("staying silent doesn't approve it").
                // Emitting an explicit `allow` instead WOULD bypass that flow,
                // which is why this is not that.
                cost_notice = Some(agent_cost::warn_reason(tokens, &cost_config));
                audit_agent_cost_warning(url, session_id, tool_name, tokens, &cost_config).await;
            }
            BudgetStatus::Exceeded | BudgetStatus::Ok => {}
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
        emit_cost_notice(&payload, cost_notice);
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
        emit_cost_notice(&payload, cost_notice);
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
pub(crate) fn payload_is_subagent_dispatch(payload: &serde_json::Value) -> bool {
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
pub(crate) fn edit_tool_target_path(tool_input: Option<&serde_json::Value>) -> Option<&str> {
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
/// What: `true` when the basename is `TASK.md`, or when any path *component* is
/// exactly `.trusty-mpm` or `.trusty-tools`. Component matching (not substring)
/// is deliberate so the crate source dir `crates/trusty-mpm/` — note no leading
/// dot — is NOT matched. A `MEMORY.md` under either component still matches, so
/// the palace index and the retired `.trusty-mpm/MEMORY.md` override stay
/// allowed.
/// Test: `is_pm_orchestration_path_matches_owned_state`.
fn is_pm_orchestration_path(path: &str) -> bool {
    use std::path::{Component, Path};
    let p = Path::new(path);
    // #5086: `MEMORY.md` dropped from the basename match — core.md (#5079)
    // forbids the PM writing it, so a bare one must not be always-allowed.
    if matches!(p.file_name().and_then(|s| s.to_str()), Some("TASK.md")) {
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
pub(crate) fn is_source_code_path(path: &str) -> bool {
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

/// Build a `hookSpecificOutput.additionalContext` body — a message TO the agent
/// that changes no decision.
///
/// Why (#4837 review, HIGH): the agent-cost warning is addressed to the agent
/// ("wrap up and report back"), but the only channel it had was an audit POST
/// to the daemon, so no agent ever read it — the graduated warn-then-stop
/// response did not exist from the agent's side. `additionalContext` is the
/// channel that fixes it: Claude Code injects the string next to the tool
/// result, and the object carries **no** `permissionDecision`, so the call
/// still goes through the normal permission flow. Per the hooks reference
/// (<https://code.claude.com/docs/en/hooks>, confirmed 2026-08-04): "Exit code
/// 0 with no output means the hook has no decision to report, so the tool call
/// continues through the normal permission flow… staying silent doesn't approve
/// it", and `additionalContext` is listed for `PreToolUse` as appearing "next
/// to the tool result". That is precisely the property the author needed and
/// could not find: context reaches the agent, and an explicit `allow` — which
/// WOULD bypass the permission flow — is never emitted.
/// What: returns the `serde_json::Value` whose compact `Display` is the single
/// stdout object; mirrors [`build_pretooluse_deny_response`]'s envelope.
/// Test: `build_pretooluse_context_response_carries_no_decision`.
pub(crate) fn build_pretooluse_context_response(context: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": context
        }
    })
}

/// Emit a pending agent-cost notice at a subagent's ALLOW exit.
///
/// Why (#4837 review, HIGH): a `PreToolUse` hook's stdout may carry exactly one
/// object, so the notice cannot be printed the moment it is computed — Guard 4's
/// opt-in persona gate still runs after that point and may print a deny. Holding
/// the text and emitting it only at the ALLOW exits keeps stdout single-object
/// and keeps that gate's behaviour byte-for-byte unchanged. The once-per-agent
/// claim happens HERE, not at composition time, so a notice that loses the race
/// to a deny is not silently spent.
/// What: no-op when `notice` is `None` or the agent has already been told;
/// otherwise prints [`build_pretooluse_context_response`].
/// Test: the claim is pinned by
/// `pm_guard_cost::warn_notice_is_claimed_once_per_agent`; the JSON shape by
/// `build_pretooluse_context_response_carries_no_decision`.
fn emit_cost_notice(payload: &serde_json::Value, notice: Option<String>) {
    let Some(text) = notice else {
        return;
    };
    if pm_guard_cost::claim_warn_notice(payload) {
        println!("{}", build_pretooluse_context_response(&text));
    }
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
///
/// Best-effort audit POST recording an agent-cost WARNING to the daemon.
///
/// Why (#4837): the warning is delivered to the AGENT via
/// [`build_pretooluse_context_response`]; this is the second, independent
/// consumer — the daemon's event stream, where the PM and dashboard read it.
/// Both are needed: the agent acts on the nudge, the PM sees which of its
/// delegations are running expensive. Fire-and-forget with the same tight
/// timeouts as [`audit_denied_tool`]: a down daemon costs the timeout and
/// nothing else, and never changes the decision.
/// What: POSTs `{session_id, event:"PreToolUse", payload:{cwd, tool,
/// pm_guard_decision:"warn", agent_cost_tokens, agent_cost_max,
/// pm_guard_reason}}` to `<url>/hooks` and drops any error.
/// Test: side-effect-only best-effort I/O; the classification that reaches it
/// is pinned by `core::agent_cost::warns_at_the_warn_threshold` and
/// `pm_guard_cost::default_config_only_warns_on_the_same_transcript`.
async fn audit_agent_cost_warning(
    url: &str,
    session_id: &str,
    tool_name: &str,
    tokens: u64,
    config: &AgentCostConfig,
) {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_default();
    // Same string the agent is shown, not a second copy that can drift from it.
    let reason = agent_cost::warn_reason(tokens, config);
    let body = serde_json::json!({
        "session_id": session_id,
        "event": "PreToolUse",
        "payload": {
            "cwd": cwd,
            "tool": tool_name,
            "pm_guard_decision": "warn",
            "agent_cost_tokens": tokens,
            "agent_cost_max": config.max_tokens,
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
        // #5086: a bare `MEMORY.md` outside a PM-owned directory must NOT match
        // either — the prompt forbids writing it (core.md, #5079).
        for p in [
            "crates/trusty-mpm/src/lib.rs",
            "src/main.rs",
            "notes.md",
            "MEMORY.md",
            "/Users/x/some/project/MEMORY.md",
            "docs/MEMORY.md",
        ] {
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

    #[test]
    fn build_pretooluse_context_response_carries_no_decision() {
        // #4837 review HIGH: this is the channel that reaches the AGENT. The
        // absence of `permissionDecision` is the whole safety property — with
        // one, the object would approve the call and bypass the permission
        // flow, which is exactly what the author declined to do.
        let v = build_pretooluse_context_response("wrap up");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "wrap up");
        assert!(
            v["hookSpecificOutput"].get("permissionDecision").is_none(),
            "an explicit decision here would bypass the permission flow"
        );
        assert!(!v.to_string().contains('\n'));
    }
}
