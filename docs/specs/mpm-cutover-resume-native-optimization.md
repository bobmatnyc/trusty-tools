# trusty-mpm Cutover: Resume Bridge + Native Optimization

**DOC-28** | Status: `Draft` | Date: 2026-06-26

## Summary

Three features enabling migration from claude-mpm (Python, deprecated) to trusty-mpm (Rust):
1. **Resume bridge** (`tm session resume`) finds paused-session files in both claude-mpm (JSON) and trusty-mpm (markdown) formats across projects, rendering context into the current conversation.
2. **Native caveman prompt optimization** injects telegraphic output-style constraints into assembled system prompts (22–87% output token reduction), matching existing trusty-agents behavior.
3. **Native ztk-style tool-call compression** implements command-domain-aware filtering of tool-call output (git, cargo, ls, grep, etc.) natively in Rust, reaching parity with the external `ztk` binary.

All three features unify under a thin `OptimizationPipeline` abstraction, with status visibility in the `tm` launch banner.

---

## Contents

- [A: Cutover Resume Bridge](#spec-mpm-cutover-01draft)
- [B: Native Caveman Prompt Optimization](#spec-mpm-cutover-02draft)
- [C: Native ztk Tool-Call Compression](#spec-mpm-cutover-03draft)
- [Unifying Glue: OptimizationPipeline + Banner](#unifying-glue-optimization-pipeline--banner)
- [Proposed Ticket Breakdown](#proposed-ticket-breakdown)
- [Open Questions / Non-Goals](#open-questions--non-goals)

---

## A: Cutover Resume Bridge {#SPEC-MPM-CUTOVER-01~draft}

### Goal

During migration, users may pause work under claude-mpm (Python tool) and resume under trusty-mpm (Rust). The `tm session resume` CLI command (and the delegating `/mpm-session-resume` skill) must discover and render paused-session context from both formats, defaulting to the current repo but optionally scanning all machine-local checkouts where claude-mpm ran.

### Behavior & Scope

**Default (no flags):** Scan the current repo only.
- Trusty-mpm native format: `.trusty-mpm/sessions/session-*.md` (markdown headers: `## Summary`, `## Completed`, `## In Progress`, `## Next Steps`, `## Git Context`) + `LATEST-SESSION.txt`.
- Claude-mpm format to parse (NEW): `.claude-mpm/sessions/session-*.json` (with fields: `session_id`, `paused_at`, `duration_hours`, `context_usage`, `conversation`, `git_context`, `active_context`, `important_reminders`, `resume_instructions`, `open_questions`, `performance_metrics`, `todos`, `task_list`, `version`, `build`, `project_path`); also `LATEST-SESSION.txt` (small human-readable pointer with lines: `Latest Session:`, `Paused At:`, `Project:`, `Files:`, `Quick Resume:`).
- Render both formats into a UNIFIED list sorted newest-first by pause time, each entry format-labeled (trusty-mpm vs claude-mpm).

**`--all-projects` flag:** Enumerate machine-wide resumable sessions via claude-mpm's own registry at `~/.claude-mpm/session-registry.db` (SQLite DB with `sessions` table: `session_id`, `project_path`, `project_name`, `started_at`, `last_active`, `status`, `pid`). Algorithm:
  1. SELECT DISTINCT `project_path` (newest `last_active` first).
  2. Filter to surviving directories (skip deleted/ephemeral paths).
  3. For each path, verify live pause file: `<path>/.claude-mpm/sessions/LATEST-SESSION.txt` and/or `<path>/.claude-mpm/sessions/session-*.json`. Pause file exists only when actually paused (ground-truth for "resumable").
  4. Collect resumable sessions, sorted newest-first by pause time. (Optionally also sweep trusty-mpm `.trusty-mpm/sessions/` machine-wide; see open questions.)

**Resume semantics:** Render paused context (digest fields: `resume_instructions`, `important_reminders`, `open_questions`, `todos`, `task_list`, `git_context`, `paused_at`, `context_usage`) into the conversation as context — do **not** dump full `conversation` field (often very large) — do **not** re-spawn a process.

### Integration Points

| File | Purpose |
|------|---------|
| `crates/trusty-mpm/src/core/claude_mpm_registry.rs` (NEW) | Read machine-wide claude-mpm registry at `~/.claude-mpm/session-registry.db` (SQLite); filter to surviving paths with live pause files. Dependency: `rusqlite`. Mark all code `// CUTOVER BRIDGE — remove post-migration (#<tracking-issue>)`. |
| `crates/trusty-mpm/src/bin/tm/commands/session/resume.rs` | Implement native session-finding (both local formats + machine-wide claude-mpm via registry) and rendering logic. |
| `crates/trusty-mpm/src/assets/skills/mpm-session-resume.md` | Skill file (bundled via `core/bundle_skills.rs:98`); today pure bash, CWD-only. Update to delegate to `tm session resume --all-projects`. |
| `crates/trusty-mpm/src/daemon/api.rs` | Existing pause/resume handler (line ~961); no changes needed for session discovery. |

### Locked Decisions

1. **Machine-wide discovery via claude-mpm registry DB:** Don't rely on `ProjectDiscovery` / `~/.claude/projects/` (only 38% coverage). Read SQLite registry directly; it is ground-truth. **Rationale:** claude-mpm projects are discovered only when Claude Code is opened with that exact dir as cwd; the registry captures all projects where claude-mpm ran, including worktrees and repos outside the Claude Code inventory.
2. **Single code path in Rust CLI:** Implement discovery + rendering in `tm session resume` (with `--all-projects` flag); the bash skill delegates to it.
3. **Claude-mpm format is temporary.** Mark all claude-mpm-DB-reading and JSON-parsing code with `// CUTOVER BRIDGE — remove post-migration (#<tracking-issue>)`.
4. **No `ProjectDiscovery` for claude-mpm.** Drop it as the discovery surface for this feature; it is no longer used.
5. **Permanence:** File a dedicated deletion-tracking GitHub issue to rip out claude-mpm parsing once migration is complete.

### Cleanup / Migration Notes

- The SQLite registry reader and JSON parser are temporary bridges; every function and block must be annotated with the deprecation comment.
- The deletion tracking issue becomes a tech-debt ticket that unblocks post-migration cleanup.
- Optional: 8 projects use older `<project>/.claude-mpm/logs/sessions/` layout (earlier claude-mpm versions); the bridge may optionally scan this path as a fallback (mark low-priority).

---

## B: Native Caveman Prompt Optimization {#SPEC-MPM-CUTOVER-02~draft}

### Goal

Reach feature parity with trusty-agents: trusty-mpm should natively inject the telegraphic "caveman" output-style constraint into its assembled system prompt. This reduces agent OUTPUT tokens by 22–87%, with no behavior change to reasoning or task completion.

### Behavior & Scope

Extract the existing `OutputStyle` enum and prompt-fragment logic from trusty-agents into a shared `trusty-agents-common` crate. Wire trusty-mpm to:
1. Consume the shared output-style module (feature-gated if needed).
2. Append the output-style fragment in `core/instruction_pipeline.rs` `assemble_system_prompt()` (today line ~71; concatenates PM_INSTRUCTIONS/WORKFLOW/AGENT_DELEGATION/BASE_PM with no optimization pass).
3. Expose configurable (which style level, on/off) consistent with trusty-agents API.

**Result:** System prompt is written to `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md` and passed to Claude via `--append-system-prompt-file`.

### Integration Points

| File | Purpose |
|------|---------|
| `crates/trusty-agents/src/compress/output_prompt.rs` | Source of truth today: `OutputStyle` enum (None/Lite/Full/Ultra), prompt-fragment constants. **Move to shared crate.** |
| `crates/trusty-agents-common/src/lib.rs` | New destination: import and re-export the `OutputStyle` module. |
| `crates/trusty-agents/src/agents/prompt_builder/mod.rs` | Update to consume from `trusty-agents-common`; line ~135 `with_output_style`. **No behavior change.** |
| `crates/trusty-agents/src/runtime/subagent_mode.rs` | Update references (line ~269). |
| `crates/trusty-agents/src/agents/in_process_runner.rs` | Update references (line ~325). |
| `crates/trusty-mpm/src/core/instruction_pipeline.rs` | **New code:** Wire output-style fragment append in `assemble_system_prompt()` (~line 71). Config sourced from `~/.trusty-mpm/framework/hooks/optimizer.toml` or CLI flag. |

### Locked Decisions

1. **Shared crate for single source of truth:** Move `OutputStyle` and fragment constants to `trusty-agents-common`; both trusty-agents and trusty-mpm import from it.
2. **Refactor trusty-agents (no behavior change):** Trusty-agents consumes the shared module; no logic change, only imports.
3. **Configurable at prompt-assembly time:** On/off and style level set via config file or CLI, consistent with trusty-agents exposure.
4. **Distinction:** This is **output-style injection** (reduces OUTPUT tokens), not the **tool-output compression** in `core/compress.rs` (`CompressionLevel::Caveman`). Both use "caveman" terminology but solve different problems.

### Parity Notes

Trusty-agents wiring today (for reference):
- `agents/prompt_builder/mod.rs:135` — `with_output_style`
- `runtime/subagent_mode.rs:269` — subagent prompt building
- `agents/in_process_runner.rs:325` — in-process runner integration

Trusty-mpm has **no** equivalent today.

---

## C: Native ztk Tool-Call Compression {#SPEC-MPM-CUTOVER-03~draft}

### Goal

Implement command-domain-aware compression of tool-call output (git, cargo, ls, grep, etc.) natively in Rust, reaching parity with claude-mpm's external `ztk` Zig binary. Keep the implementation pure-Rust, in-tree, with no external binary dependencies.

### Behavior & Scope

Add command-domain-aware filters as a new tier/mode in the existing `crates/trusty-mpm/src/core/compress.rs` (`CompressionLevel` today: Off/Trim/Summarise/Caveman). Wire at the single existing seam: `daemon/optimizer.rs` `optimize_tool_output`, called from `daemon/mcp_backend.rs:183-192` (PostToolUse hook, before ring buffer).

Filters recognize command patterns (e.g., `git diff`, `cargo build`, `ls -la`, `grep`) and apply domain-specific truncation:
- **git:** Strip diff hunks beyond first N lines, summarize file counts
- **cargo:** Collapse warning/error repetition, keep summary
- **ls:** Truncate long listings, keep header/summary
- **grep:** Keep only match context, truncate repetition

Configuration remains in `~/.trusty-mpm/framework/hooks/optimizer.toml`; default level `Trim`.

### Integration Points

| File | Purpose |
|------|---------|
| `crates/trusty-mpm/src/core/compress.rs` | Extend `CompressionLevel` enum and add command-domain-aware filter implementations. |
| `crates/trusty-mpm/src/daemon/optimizer.rs` | `optimize_tool_output()` — single seam, calls compression filters. Config from `optimizer.toml`. |
| `crates/trusty-mpm/src/daemon/mcp_backend.rs:183-192` | Single call site: PostToolUse hook before ring buffer. **No changes to call site.** |
| `crates/trusty-mpm/src/core/config.rs` | Add compression tier/level to config struct if not already present. |

### Locked Decisions

1. **Native in-tree filters only:** Do **not** shell out to or bundle the external `codejunkie99/ztk` Zig binary. Keep it pure-Rust, single-install clean.
2. **Extend existing abstraction:** Add filters as a new mode/tier in `CompressionLevel` enum and wired at the single existing seam (`optimize_tool_output`).
3. **Domain-aware (not regex-only):** Recognize command type (git, cargo, ls, grep) and apply targeted truncation rules, not generic line-count caps.

### Parity Notes

Background research lives at `docs/trusty-agents/research/token-compression-rtk-ztk.md`. The `ztk` tool reference (being replaced) is external; this feature makes it native.

---

## Unifying Glue: OptimizationPipeline + Banner

### OptimizationPipeline Abstraction

Today compression is one ad-hoc call site with no abstraction. Introduce a thin `OptimizationPipeline` struct in `crates/trusty-mpm/src/core/` that **both** the system-prompt pass (B) and the tool-call relay (C) run through:

```rust
pub struct OptimizationPipeline {
    pub output_style: Option<OutputStyle>,
    pub compression_level: CompressionLevel,
}

impl OptimizationPipeline {
    pub fn apply_to_prompt(&self, prompt: &str) -> String { … }
    pub fn apply_to_tool_output(&self, output: &str, command: &str) -> String { … }
}
```

This is **not** gold-plated up front but should be introduced/refined as B and C land. It enables future enhancements (e.g., multi-pass optimization) and centralizes config.

### Launch Banner Status Visibility

The `tm` launch banner lives at `crates/trusty-mpm/src/bin/tm/formatters/banner.rs`, `render_launch_banner()` (~line 105). The claude-mpm banner displays `⚡ ztk compression: on (v0.3.1)`. Update the trusty-mpm banner to surface:

```
⚡ Optimization: caveman + ztk compression enabled
```

This makes optimization status visible to users during startup, reaching parity with claude-mpm's transparency.

---

## Proposed Ticket Breakdown

| Issue | Title | Depends On | Notes |
|-------|-------|-----------|-------|
| #TBD-A | Cutover: Resume bridge (claude-mpm JSON + registry DB + `--all-projects` flag) | — | Standalone; implement `tm session resume --all-projects`. Add `rusqlite` dependency; new `claude_mpm_registry.rs` module. Mark claude-mpm JSON reader + registry reader `// CUTOVER BRIDGE`. |
| #TBD-A-CLEANUP | Post-Migration: Remove claude-mpm JSON parsing from resume bridge | #TBD-A | Tech-debt tracking ticket; unblocks deletion post-migration. |
| #TBD-B | Native caveman prompt optimization (shared OutputStyle + trusty-mpm wiring) | — | Extract `OutputStyle` to `trusty-agents-common`; update trusty-agents imports; wire trusty-mpm. Recommend isolated worktree. |
| #TBD-C | Native ztk-style tool-call compression (command-domain-aware filters) | — | Extend `CompressionLevel`; add domain-aware filter logic; no call-site changes. Recommend isolated worktree. |
| #TBD-UNIFY | OptimizationPipeline abstraction + banner status (B + C refinement) | #TBD-B, #TBD-C | Follow-on; refine after B and C land. |

**Sequencing:**
- **#TBD-A** (resume bridge) is independent and should land first (enables user workflows during cutover).
- **#TBD-B** and **#TBD-C** touch trusty-mpm compression/optimization code; isolate in separate worktrees to avoid conflicts.
- **#TBD-UNIFY** lands after B and C stabilize; consolidates abstraction.
- **#TBD-A-CLEANUP** is a future tech-debt ticket, filed once migration is underway.

---

## Open Questions / Non-Goals

### Open Questions

1. **Dual-format collision (RESOLVED):** When both trusty-mpm and claude-mpm sessions exist in a repo, present unified list sorted newest-first by pause time, each entry format-labeled. No silent preference needed.
2. **`--all-projects` scope for trusty-mpm:** Should the flag ALSO sweep trusty-mpm `.trusty-mpm/sessions/` across machine-local checkouts, or is claude-mpm registry sweep sufficient for cutover? Recommend: Primarily claude-mpm (cutover focus); trusty-mpm machine-wide sweep is optional secondary feature for v1.
3. **Registry DB performance:** On machines with ~850 distinct `project_path` values in the registry, is stat + read-one-file filtering fast enough (recommend <1s total)? This is low-risk; path existence check and single `LATEST-SESSION.txt` read are cheap operations.
4. **Older layout variant:** Should the bridge also check `<project>/.claude-mpm/logs/sessions/` (used by ~8 projects on older claude-mpm versions)? Recommend: Optional, low-priority fallback for v1; mark as nice-to-have post-cutover.
5. **OutputStyle config source for trusty-mpm (Deliverable B):** CLI flag, config file, env var, or combination? Recommend: Config file (`optimizer.toml`), with env-var override for CI/testing.
6. **ztk filter granularity (Deliverable C):** How many commands to recognize in v1 (git, cargo, ls, grep only) vs. future expansion? Recommend: v1 covers git/cargo/ls/grep; future PRs add more domains.
7. **OptimizationPipeline scope:** Should it also orchestrate logging/telemetry for optimization effectiveness? Defer to post-B/C refinement phase.

### Non-Goals

- **Full ztk parity:** This is a functional replacement for token reduction, not a feature-for-feature clone. Edge cases and esoteric ztk options can be added incrementally post-launch.
- **Backward compatibility with claude-mpm configs:** Trusty-mpm uses its own `optimizer.toml` format; no migration of claude-mpm config files is in scope.
- **Optimization ablation studies:** Measuring actual token savings in production is important but separate; this spec defines the feature contract, not the measurement strategy.

---

## References

- [Trusty-agents caveman optimization (existing)](../../../crates/trusty-agents/src/compress/output_prompt.rs)
- [Trusty-mpm compression seam](../../../crates/trusty-mpm/src/daemon/optimizer.rs)
- [ProjectDiscovery API](../../../crates/trusty-mpm/src/core/project_discovery.rs)
- [Token compression research](../../../docs/trusty-agents/research/token-compression-rtk-ztk.md)
