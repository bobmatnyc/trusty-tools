# 0025. Collapse the agent and skill tier hierarchies: one deploy target, many declared sources, precedence resolved at deploy time

- **Status:** Proposed
- **Date:** 2026-08-01
- **Scope:** crates `trusty-mpm` and `trusty-agents-common` — `agents::{deployer,manifest,tier_audit}`, `skills::{deployer,manifest,tiers}`, `core::paths`, and the session-launch / install / sync-assets provisioning paths
- **Reversibility Cost:** High
- **Decision Drivers:** three unreconciled write paths for the same asset classes, the #4408 silent-shadow incident, migration data-loss risk, precedence resolved by directory order rather than a declared source list
- **Supersedes / Superseded by:** Supersedes #387 (closed 2026-08-01 as superseded by this ADR)

> **DRAFT NOTE (2026-08-01):** The owner reversed the (B) ruling this section
> currently records ("one global canonical agent deploy target, no
> project-scoped agents"). Replacement ruling (C): agents and skills split
> into **universal** (global, everyone gets them — the (B) behaviour below)
> and **scaffolded** (deployed into a per-project store selected by detected
> stack). The Decision section below is PENDING REDESIGN around this split
> and must not be treated as final. Context, Consequences-so-far, and Related
> Decisions vetting are unaffected and stand as written.

## Context

Assets deploy to **three unconnected destinations**, written by three different callers, with no single resolver:

1. `~/.claude/skills` — written by `tm install`, which builds `FrameworkPaths::default()` and targets `claude_skills_dir()`, always `dirs::home_dir()/.claude/skills` with no `$CLAUDE_CONFIG_DIR` awareness. On a live machine: 293 entries, oldest dating to April, mostly the operator's personal skills, not trusty-mpm's. This destination exists only because `tm install` builds `FrameworkPaths::default()` rather than the trusty configuration root — it is an unintended consequence of a default, not a designed tier.
2. `$CLAUDE_CONFIG_DIR/{agents,skills}` — written only by `core::standalone::global_config::ensure_global_config_dir` (`global_config.rs:78,122-125`), reached solely from the standalone `tm register` / `tm load` / `tm run` commands. Never from `tm install`, never from the daemon. Agents and skills deploy here symmetrically by the same call (42 and 55 entries live).
3. `<worktree>/.claude/{agents,skills}` — written at daemon session launch. Every daemon spawn site (`daemon/managed_routes/lifecycle.rs:963,1087`, `provisioner/workspace.rs:894`, `launch_on_main.rs:162`, `sync_assets.rs:145`) builds `FrameworkPaths::for_managed_workspace(&worktree)`, overriding `claude_agents`/`claude_skills` to the worktree. Deliberate, documented at `lifecycle.rs:959-963` citing issue #1931: the harness cwd for a managed session IS the worktree.

These are not tiers of one system — they are three write paths with different owners, different triggers, and nothing reconciling them. An operator cannot answer "which copy of this skill is my session running" without knowing which code path provisioned it.

Within the daemon path, agents and skills are scoped differently, and that is #4409's deliberate doing: `for_managed_workspace` delegates to `for_managed_project`, which overrides `claude_agents`/`claude_skills` but leaves `agent_deploy` alone — pinned by the test `agent_deploy_dir_is_not_project_local_for_managed_workspace`. At launch, `prepare_session` deploys agents to the GLOBAL `fw.agent_deploy_dir()` (`session_launch/mod.rs:562`), retracts the worktree's `.claude/agents` (`:599`), and deploys skills to the PER-WORKTREE `fw.claude_skills_dir()` (`:688`).

The #4408 incident is the sharpest evidence of the cost: a 32-byte project-tier stub whose body was the literal string `v1` beat the real 25KB `rust-engineer`. Delegation kept "working" against a content-free agent. `tm doctor` missed it because it counted files, not content. Claude Code resolves on the frontmatter `name:` field, not the filename stem, so such a shadow is undetectable without reading every file. Namespacing is unavailable to us: Claude Code's only namespacing primitive is colon-scoped `plugin:name`, reserved for plugins, and `name:` may not contain a colon — the `vercel:*` marketplace skills are the control group, colon-namespaced and consequently unable to shadow anything. Resolution order is not ours to change; we control what trusty-mpm writes and where.

Prior art: claude-mpm (the repo owner's earlier project) had the same PROJECT > REMOTE > USER > SYSTEM stack and removed it, collapsing to one target fed by prioritised git sources with numeric `priority` (lower wins), `doctor` flagging "Priority conflicts detected", and every override logged aloud as `mycompany/agents (priority: 50): engineer.md (overrides system engineer)`. It also dropped a richer frontmatter identity schema — `agent_id`, `author`, `schema_version` did not survive.

Known tracked gap, recorded not re-opened: the project-local deploy is one-shot at session launch (`core/session_assets.rs:1-30`, issues #2002/#2444), so a long-lived session's `.claude/skills` drifts stale; surfaced by `tm sessions ls`'s `[stale-assets]` marker, repaired by `tm sessions sync-assets`.

## Decision

The owner ruled Option (B) on 2026-08-01:

1. **One global canonical agent deploy target. NO project-scoped agents.** #4409 stands untouched; `agent_deploy_dir_is_not_project_local_for_managed_workspace` remains valid and must not be weakened. Projects customize agents ONLY through `manifest.toml` selection and user-tier agents. That a project cannot ship its own agent is a deliberate accepted cost. Per-project canonical agent directories are explicitly deferred to a follow-on ADR if they prove necessary.
2. Skills are already per-worktree scoped via `for_managed_workspace`; no change needed there.
3. Assets come from declared sources with explicit numeric priority, declared in `<root>/agents-sources.toml`, lower wins: project (10), user (30), catalog (50), bundled (90).
4. Precedence is resolved by pure planners with DIFFERENT identity keys by design: agents key on `agents::tier_audit::agent_identity` (frontmatter `name:`, else stem); skills key on the stem (`skills::deployer::skill_stem`). Adding frontmatter-name resolution to skills is explicitly rejected — it would import the agent hazard into a subsystem that does not have it. Agent identity is decoupled from the filename, so a shadow is invisible without reading every file; skill identity IS the filename, so a shadow is visible to `ls`. That is why #4408 happened to agents and has no skill counterpart.
5. A priority tie is a HARD ERROR — reported by `tm doctor`, affected identities refuse to deploy, existing files untouched. Resolution by map-iteration order is #4408 with extra steps.
6. Every override is visible on four surfaces: deploy-time log naming winner/loser/priority; `source` and `overrides` fields on the ledger entries; a `tm doctor` override table; an injected provenance comment in the deployed file.
7. A reserved-name table guards harness built-ins for skills. The existing `/mcp` guard (#2186) generalises from a hard-coded substring to a table checked at deploy AND creation time. The current `stem.to_lowercase().contains("mcp")` is replaced by exact-stem matching — substring matching rejects legitimate names.
8. `Origin::Project` records provenance and MUST NOT satisfy `Origin::is_framework_owned` — that predicate gates `retract_framework_agents`, which runs on every session launch.
9. `tm-agent-manager` refuses at creation time to produce an agent whose name collides with a bundled one (tracked as #4545).
10. Migration is report-first and never deletes: first run audits read-only, prints a plan, stops. `tm assets migrate` applies by copy-then-quarantine (rename with undo receipt, per #4448), never deletion. A ledger-proven `TierOwnership::UserOwned` asset is never touched automatically. A corrupt ledger refuses the operation. Migration covers three destinations and states for each whether it is swept, reconciled, or untouched: `~/.claude/skills` UNTOUCHED (and see clause 11 — the write path into it is separately removed, which is not part of the migration); `$CLAUDE_CONFIG_DIR/*` reconciled; `<worktree>/.claude/agents` swept (already is).
11. **trusty-mpm neither writes to nor reads from `~/.claude/skills`.**
    - **The write path is removed.** `tm install` currently deploys bundled skills there by building `FrameworkPaths::default()` and targeting `claude_skills_dir()` → `dirs::home_dir()/.claude/skills` (`bin/tm/commands/install.rs:162-172`, `core/paths.rs:108-135`). That write is the defect, not merely an inconvenience: it puts framework assets into the operator's personal Claude Code directory, which no trusty-mpm session ever reads. trusty-mpm has its own configuration directory and that is the only place framework assets belong.
    - **The directory itself is never swept, never quarantined, and never reported as misplaced.** Its 293 entries are the operator's own personal skills dating to April. Reporting them as misplaced invites someone to write the cleanup. Untouched means untouched.
    - Note the asymmetry that makes this urgent: the agent path has a structural guard binding retraction to the workspace path (`session_launch/mod.rs:589-598`) precisely so it can never reach the operator's real `~/.claude`. The skill path has no equivalent, and one must exist before any skill sweep ships.
    - **Historical writes stay.** trusty-mpm has already written bundled skills into `~/.claude/skills` on existing machines, intermixed with the ~293 personal entries. Those tm-written entries are not reconciled, cleaned up, or swept — not now, not later. They are inert (nothing reads them) and leaving them costs nothing. The directory is the operator's personal directory; trusty-mpm's relationship to it is simply: don't use it.
12. Selection remains a separate complementary layer: `manifest.toml`'s `[agents]`/`[skills]` include/exclude decides WHICH names deploy; this ADR decides WHICH COPY wins. Selection runs first.

## Consequences

**Easier:** the #4408 class becomes impossible in steady state; adding a source becomes a config entry; `tm doctor` can state provenance; #4448's blocker dissolves; removing the `~/.claude/skills` write path shrinks the fragmentation from three destinations to two, with no migration risk, because nothing reads what it wrote.

**Harder:** migration is the dominant risk and it is a data-loss risk; the skill migration is more dangerous than the agent one because the agent path has a guard the skill path lacks; Claude Code's precedence still favours a directory we deliberately empty, so the #4442 doctor probe becomes PERMANENT ENFORCEMENT rather than a transitional check; `Origin::Project` is a live footgun while `is_framework_owned` gates a delete path; following claude-mpm we do NOT add a richer frontmatter identity schema; orphan retraction (#391) becomes a dependency.

## Related Decisions

Vetted against prior ADRs on 2026-08-01:

- **ADR-0002 (single-install convention):** Consistent — extends the same instinct from binaries to assets.
- **ADR-0008 (project identity):** Extends — must not introduce a second way to name a project.
- **ADR-0015 (three-product agent composition):** Consistent — ADR-0015 governs FORMAT, this governs SOURCING and DEPLOYMENT for trusty-mpm only.
- **ADR-0020 (session-owned worktrees):** Consistent, and a deliberate borrowing — its "owner-unknown candidates are NEVER auto-deleted" is the same rule as clause 10. If they ever diverge, ADR-0020's formulation is senior.
- **ADR-0023 (worktree authority):** Consistent, with one gap stated openly — ADR-0023 clause 4 requires the ownership index be REBUILDABLE from ground truth. Neither the agent nor the skill ledger is rebuildable today; a lost manifest permanently reclassifies every managed file as untracked, which is #4408's shape reached by data loss. This ADR does not close that gap and does not claim to; filed as follow-up.
- **ADR-0024 (assistants as L0 delegators):** Consistent, orthogonal axis. Caution: its clause-4 editable sub-agent whitelist must consume this ADR's resolved roster rather than re-deriving one.
- **Skills have no prior ADR** — this is the first.
- **DOC-31 (`docs/specs/system-project-agents-skills.md`):** Superseded in part — §191 names committed project-tier agents in `.claude/agents/` as supported, which clause 1 and clause 2 remove. DOC-31 must be rewritten in the change set that lands those clauses.
- Note (not fixed here): main has an ADR-0021 numbering collision (`0021-cargo-bin-policy.md` and `0021-slack-inbound-hybrid-gateway-eventstream.md`) needing separate cleanup with `INDEX.md` reconciled.
