# 0025. Collapse the agent and skill tier hierarchies: one deploy target, many declared sources, precedence resolved at deploy time

- **Status:** Proposed
- **Date:** 2026-08-01
- **Scope:** crates `trusty-mpm` and `trusty-agents-common` — `agents::{deployer,manifest,tier_audit}`, `skills::{deployer,manifest,tiers}`, `core::paths`, and the session-launch / install / sync-assets provisioning paths
- **Reversibility Cost:** High
- **Decision Drivers:** three unreconciled write paths for the same asset classes, the #4408 silent-shadow incident, migration data-loss risk, precedence resolved by directory order rather than a declared source list
- **Supersedes / Superseded by:** Supersedes #387 (closed 2026-08-01 as superseded by this ADR)

## Context

Assets deploy to **three unconnected destinations**, written by three different callers, with no single resolver:

1. `~/.claude/skills` — written by `tm install`, which builds `FrameworkPaths::default()` and targets `claude_skills_dir()`, always `dirs::home_dir()/.claude/skills` with no `$CLAUDE_CONFIG_DIR` awareness. On a live machine: 293 entries, oldest dating to April, mostly the operator's personal skills, not trusty-mpm's. This destination exists only because `tm install` builds `FrameworkPaths::default()` rather than the trusty configuration root — it is an unintended consequence of a default, not a designed tier.
2. `$CLAUDE_CONFIG_DIR/{agents,skills}` — written only by `core::standalone::global_config::ensure_global_config_dir` ([`global_config.rs:78`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/standalone/global_config.rs#L78), [`global_config.rs:122-125`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/standalone/global_config.rs#L122-L125)), reached solely from the standalone `tm register` / `tm load` / `tm run` commands. Never from `tm install`, never from the daemon. Agents and skills deploy here symmetrically by the same call (42 and 55 entries live).
3. `<worktree>/.claude/{agents,skills}` — written at daemon session launch. Every daemon spawn site ([`daemon/managed_routes/lifecycle.rs:974`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs#L974), [`daemon/managed_routes/lifecycle.rs:1289`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs#L1289), [`provisioner/workspace.rs:905`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/provisioner/workspace.rs#L905), [`launch_on_main.rs:138`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/daemon/managed_routes/launch_on_main.rs#L138), [`sync_assets.rs:149`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/daemon/managed_routes/sync_assets.rs#L149)) builds `FrameworkPaths::for_managed_workspace(&worktree)`, overriding `claude_agents`/`claude_skills` to the worktree. Deliberate, documented at [`lifecycle.rs:970-974`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs#L970-L974) citing issue #1931: the harness cwd for a managed session IS the worktree.

These are not tiers of one system — they are three write paths with different owners, different triggers, and nothing reconciling them. An operator cannot answer "which copy of this skill is my session running" without knowing which code path provisioned it.

Within the daemon path, agents and skills are scoped differently, and that is #4409's deliberate doing: `for_managed_workspace` delegates to `for_managed_project`, which overrides `claude_agents`/`claude_skills` but leaves `agent_deploy` alone — pinned by the test `agent_deploy_dir_is_not_project_local_for_managed_workspace`. At launch, `prepare_session` deploys agents to the GLOBAL `fw.agent_deploy_dir()` ([`session_launch/mod.rs:562`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/session_launch/mod.rs#L562)), retracts the worktree's `.claude/agents` ([`session_launch/mod.rs:599`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/session_launch/mod.rs#L599)), and deploys skills to the PER-WORKTREE `fw.claude_skills_dir()` ([`session_launch/mod.rs:688`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/session_launch/mod.rs#L688)).

The #4408 incident is the sharpest evidence of the cost: a 32-byte project-tier stub whose body was the literal string `v1` beat the real 25KB `rust-engineer`. Delegation kept "working" against a content-free agent. `tm doctor` missed it because it counted files, not content. Claude Code resolves on the frontmatter `name:` field, not the filename stem, so such a shadow is undetectable without reading every file. Namespacing is unavailable to us: Claude Code's only namespacing primitive is colon-scoped `plugin:name`, reserved for plugins, and `name:` may not contain a colon — the `vercel:*` marketplace skills are the control group, colon-namespaced and consequently unable to shadow anything. Resolution order is not ours to change; we control what trusty-mpm writes and where.

Prior art: claude-mpm (the repo owner's earlier project) had the same PROJECT > REMOTE > USER > SYSTEM stack and removed it, collapsing to one target fed by prioritised git sources with numeric `priority` (lower wins), `doctor` flagging "Priority conflicts detected", and every override logged aloud as `mycompany/agents (priority: 50): engineer.md (overrides system engineer)`. It also dropped a richer frontmatter identity schema — `agent_id`, `author`, `schema_version` did not survive.

Known tracked gap, recorded not re-opened: the project-local deploy is one-shot at session launch ([`core/session_assets.rs:1-30`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/session_assets.rs#L1-L30), issues #2002/#2444), so a long-lived session's `.claude/skills` drifts stale; surfaced by `tm sessions ls`'s `[stale-assets]` marker, repaired by `tm sessions sync-assets`.

## Decision

The owner ruled on 2026-08-01. Final ruling on the last open question, verbatim: "agreed on manifest.toml" — extend the existing `manifest.toml`, do NOT add a second file format.

1. **Classification is declared per project, in `manifest.toml`.** Each project carries `[agents]`/`[skills]` `include`/`exclude` naming the assets it uses. The classification is a property of the PROJECT, not of the catalog, so it lives with the project — this supersedes any central table. #4409 stands untouched; `agent_deploy_dir_is_not_project_local_for_managed_workspace` remains valid and must not be weakened. A project cannot ship its own agent FILE — it can only select, by name, from the catalog `manifest.toml` resolves against. Per-project canonical agent directories (a project authoring its own agent content) are explicitly deferred to a follow-on ADR if they prove necessary. Skills stay per-worktree scoped via `for_managed_workspace`, unchanged.

   **1a. Why `manifest.toml` and not claude-mpm's file.** claude-mpm's `agents-manifest.yaml` is a three-field format stamp (`repo_format_version`, `min_cli_version`, `migration_notes`) living in the agent repository; it performs no selection. claude-mpm's real selection lives in per-project `.claude-mpm/configuration.yaml`, whose `agent_deployment` and `agent_sync` blocks `manifest.toml` already mirrors. The full mapping: `excluded_agents` → `[agents] exclude`; skill selection → `[skills] include/exclude`; `filter_non_mpm_agents` + `mpm_author_patterns` → `Origin::is_framework_owned`, which is STRICTLY STRONGER because it reads positive provenance from the deploy ledger rather than pattern-matching an author string a user can forge or omit.

   **1b. The one gap, and the schema change that closes it.** `ContentSource` (`Bundled|Catalog`) cannot express claude-mpm's multi-source numeric priority. Replace it with a source list carrying `id`, `path`, `priority`, `enabled`. Additive and contained: a one-entry list reproduces today's behaviour exactly.

2. **The manifest is created once, on request, by `tm-agent-manager`.** When a project has no `manifest.toml`, tm does not silently invent one — it requests creation. `tm-agent-manager` runs stack detection, proposes an agent and skill set, and writes the manifest. It is the SAME component that refuses bundled-name collisions at creation time (#4545) and the same mechanism proposed by #4528. #4528 becomes the implementation vehicle; #4545 folds into it as its collision-refusal requirement — one component, not three designs.

3. Assets come from declared sources with explicit numeric priority, declared in `<root>/agents-sources.toml`, lower wins: project (10), user (30), catalog (50), bundled (90).

4. Precedence is resolved by pure planners with DIFFERENT identity keys by design: agents key on `agents::tier_audit::agent_identity` (frontmatter `name:`, else stem); skills key on the stem (`skills::deployer::skill_stem`). Adding frontmatter-name resolution to skills is explicitly rejected — it would import the agent hazard into a subsystem that does not have it. Agent identity is decoupled from the filename, so a shadow is invisible without reading every file; skill identity IS the filename, so a shadow is visible to `ls`. That is why #4408 happened to agents and has no skill counterpart.

5. A priority tie is a HARD ERROR — reported by `tm doctor`, affected identities refuse to deploy, existing files untouched. Resolution by map-iteration order is #4408 with extra steps.

6. Every override is visible on four surfaces: deploy-time log naming winner/loser/priority; `source` and `overrides` fields on the ledger entries; a `tm doctor` override table; an injected provenance comment in the deployed file.

7. A reserved-name table guards harness built-ins for skills. The existing `/mcp` guard (#2186) generalises from a hard-coded substring to a table checked at deploy AND creation time. The current `stem.to_lowercase().contains("mcp")` is replaced by exact-stem matching — substring matching rejects legitimate names.

8. `Origin::Project` records provenance and MUST NOT satisfy `Origin::is_framework_owned` — that predicate gates `retract_framework_agents`, which runs on every session launch.

9. **Startup reconciliation extends `retract_framework_agents`; it is not a new sweep.** ⚠️ **Highest-risk clause in this ADR.** At session launch, deployed agents are compared against the manifest. A deployed agent absent from the manifest is removed IF AND ONLY IF `Origin::is_framework_owned` is true for its ledger entry. A non-framework entry is left in place as user-owned. An UNTRACKED file is never touched. This reuses the existing predicate and select-seam verbatim, with the manifest supplying the predicate. It is the only clause that deletes files as a routine part of every session launch, and its blast radius is every agent on the machine if the manifest is misread. A missing, empty, or unparseable manifest MUST fail closed — reconcile nothing.

10. **Stack detection runs ONCE, at manifest creation — not per launch.** The manifest is stable by design and changes only on explicit user or PM request. Detection reuses `language_agent_scope`'s marker probes (`core/manifest/project_lang.rs`); a skills equivalent of that module is on the critical path, not a follow-on. Auto-re-detection at launch is explicitly REJECTED — a manifest that silently rewrites itself is not a declaration.

11. **Drift is reported, never auto-corrected.** When detected markers disagree with the manifest, `tm doctor` reports the discrepancy and names the command to update it. This reporting is the compensating control replacing per-launch re-detection; without it, clause 10 has no safety net. Accepted residual, stated explicitly: between a stack change and someone running `tm doctor`, the project is mis-scaffolded and nothing says so at launch.

12. **Delegation to an unresolvable agent must fail LOUDLY.** A manifest that omits an agent is now the primary way an agent goes missing, and omission is the manifest's normal operating mode rather than an error. This converts "every agent always exists" from an invariant into an assumption; the loud-failure check is what replaces it. No such check was found in the delegation path today — current behaviour on an unresolvable name is UNVERIFIED (see Consequences).

13. `tm-agent-manager` refuses at creation time to produce an agent whose name collides with a bundled one (tracked as #4545, folded into #4528 per clause 2).

14. Migration is report-first and never deletes: first run audits read-only, prints a plan, stops. `tm assets migrate` applies by copy-then-quarantine (rename with undo receipt, per #4448), never deletion. A ledger-proven `TierOwnership::UserOwned` asset is never touched automatically. A corrupt ledger refuses the operation. Migration covers three destinations and states for each whether it is swept, reconciled, or untouched: `~/.claude/skills` UNTOUCHED (and see clause 15 — the write path into it is separately removed, which is not part of the migration); `$CLAUDE_CONFIG_DIR/*` reconciled; `<worktree>/.claude/agents` swept (already is).

15. **trusty-mpm neither writes to nor reads from `~/.claude/skills`.**
    - **The write path is removed.** `tm install` currently deploys bundled skills there by building `FrameworkPaths::default()` and targeting `claude_skills_dir()` → `dirs::home_dir()/.claude/skills` ([`bin/tm/commands/install.rs:162-172`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/bin/tm/commands/install.rs#L162-L172), [`core/paths.rs:113-137`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/paths.rs#L113-L137) (`default()`), [`core/paths.rs:433-435`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/paths.rs#L433-L435) (`claude_skills_dir()`)). That write is the defect, not merely an inconvenience: it puts framework assets into the operator's personal Claude Code directory, which no trusty-mpm session ever reads. trusty-mpm has its own configuration directory and that is the only place framework assets belong.
    - **The directory itself is never swept, never quarantined, and never reported as misplaced.** Its 293 entries are the operator's own personal skills dating to April. Reporting them as misplaced invites someone to write the cleanup. Untouched means untouched.
    - Note the asymmetry that makes this urgent: the agent path has a structural guard binding retraction to the workspace path ([`session_launch/mod.rs:589-598`](https://github.com/bobmatnyc/trusty-tools/blob/fd15c589b6c92d03ebcfb8a37463611300a1b1b4/crates/trusty-mpm/src/core/session_launch/mod.rs#L589-L598)) precisely so it can never reach the operator's real `~/.claude`. The skill path has no equivalent, and one must exist before any skill sweep ships.
    - **Historical writes stay.** trusty-mpm has already written bundled skills into `~/.claude/skills` on existing machines, intermixed with the ~293 personal entries. Those tm-written entries are not reconciled, cleaned up, or swept — not now, not later. They are inert (nothing reads them) and leaving them costs nothing. The directory is the operator's personal directory; trusty-mpm's relationship to it is simply: don't use it.

16. Selection remains a separate complementary layer: `manifest.toml`'s `[agents]`/`[skills]` include/exclude decides WHICH names deploy; this ADR decides WHICH COPY wins. Selection runs first.

## Consequences

**Easier:** the #4408 class becomes impossible in steady state; adding a source becomes a config entry; `tm doctor` can state provenance; #4448's blocker dissolves; removing the `~/.claude/skills` write path shrinks the fragmentation from three destinations to two, with no migration risk, because nothing reads what it wrote; project customization reuses one file format instead of inventing a second.

**Harder:** migration is the dominant risk and it is a data-loss risk; the skill migration is more dangerous than the agent one because the agent path has a guard the skill path lacks; Claude Code's precedence still favours a directory we deliberately empty, so the #4442 doctor probe becomes PERMANENT ENFORCEMENT rather than a transitional check; `Origin::Project` is a live footgun while `is_framework_owned` gates a delete path; following claude-mpm we do NOT add a richer frontmatter identity schema; orphan retraction (#391) becomes a dependency; the manifest becomes a per-project maintenance obligation; silent absence (an agent simply not in the manifest) is a live failure class that did not exist before, and it is the manifest's NORMAL operating mode, not an edge case; drift between detected markers and the manifest is now possible and only `tm doctor` surfaces it — between a stack change and the next `tm doctor` run, a project can be silently mis-scaffolded; whether an unresolvable-agent delegation fails loudly today is UNVERIFIED, and clause 12 requires closing that gap before this ADR's assumptions hold in practice.

## Slices

Implementation follows the manifest-first re-stack. Two ordering facts govern the sequence and must not be reordered:

- **Slice 4 — the loud-failure check (clause 12) — ships at the front.** Every selection-related slice that follows leans on it: once a manifest can omit an agent as its normal operating mode, nothing downstream is safe to build until an unresolvable delegation target fails loudly instead of silently.
- **Slice 12 — startup reconciliation (clause 9).** Gated on slices 4, 11, and 13 being green **in production**, not merely merged — proportionate to it being the highest-risk clause in this ADR, and each requires its own critic pass before slice 12 is allowed to ship.

The full 21-slice sequencing (slices 1–3, 5–10, 13–21) is tracked in the implementation plan rather than reproduced here; this ADR records the two ordering constraints that are load-bearing for review, not the complete task breakdown.

## 2026-08-03 Addendum: Manifest-Based Project Configuration and the Four-Category Agent Model

**Status of this addendum:** the parent ADR stays **Proposed** — nothing here
is self-promoted to Accepted. These are owner decisions recorded 2026-08-03,
targeting the 1.3.4 release ("firm up agents and skills"). This addendum adds
detail; it does not silently reword clauses 1–16 above. Two places below
name a direct tension with clause 1 rather than resolving it — see
§Open Questions.

### B1. The manifest is `manifest.toml` — confirms clause 1, no schema change

Re-affirms the 2026-08-01 ruling verbatim ("agreed on manifest.toml"). No new
file format, and **no schema change is required**: `HarnessManifest`
(`crates/trusty-mpm/src/core/manifest/schema.rs`) already carries both
`[agents]` and `[skills]` sections in one document, resolved project > user >
catalog > compiled-default (`resolve.rs::resolve_manifest`). The owner's
2026-08-03 decision that "ONE manifest covers BOTH agents and skills" is
**already true of shipped code** — recorded here as a confirmed non-requirement,
not new work.

New requirements this addendum adds on top of the existing schema:

- **B1.1.** Every project gets its own committed `<project>/.trusty-mpm/manifest.toml`. Audience: developers working on that project, not an end-user-facing artifact. Confirmed not committed anywhere in this repo today (`git ls-tree -r origin/main --name-only | grep manifest\.toml$` → no hits).
- **B1.2.** The file is written by a scaffolding path — `tm-agent-manager`, on request — never hand-authored as the primary path (clause 2, unchanged). **This scaffolding path does not exist in code today** (`grep -rln "tm-agent-manager" crates/ --include='*.rs'` → zero hits; the name exists only as the bundled subagent persona `crates/trusty-mpm/src/assets/agents/mpm-agent-manager.md`). Tracked as issue #4528 (open, **no milestone assigned** as of this writing) with #4545 (collision refusal) folded in per clause 2/13.

### B2. tm-agent-manager's purpose: composition, not mere file creation

Claude Code's own `/agents` already lets an operator create a custom agent —
that path is legitimate and trusty-mpm neither owns nor blocks it.
`tm-agent-manager`'s distinct value is that it **builds** a custom agent by
**composing** from the `BASE_*` inheritance fragments, which `/agents` does not
do: a `/agents`-authored file is a bare agent with none of the framework's
shared behavior; a `tm-agent-manager`-authored one inherits it (git workflow,
memory routing, output format, handoff protocol, proactive-quality — whatever
the relevant `BASE_*` fragment carries).

**Verified against code, live today:**

- The five base fragments, exactly: `BASE-AGENT.md`, `BASE-ENGINEER.md`,
  `BASE-OPS.md`, `BASE-QA.md`, `BASE-RESEARCH.md`
  (`crates/trusty-mpm/src/assets/agents/BASE-*.md`; corrects this
  investigation's own earlier miscount of "4").
- Composition is a **real, live Rust mechanism**, not prose: `compose_agent`
  (`crates/trusty-agents-common/src/agents/builder.rs`) loads a source `.md`,
  walks its `extends:` chain base-first (e.g. `engineer.md` declares
  `extends: base-engineer`), strips intermediate frontmatter, and flattens the
  chain into one self-contained document with cycle/depth protection
  (`MAX_DEPTH`). Every bundled agent already goes through this at deploy time
  (`deployer.rs` calls `compose_agent`/`source_chain`).
- What is **not** built: a code path that lets `tm-agent-manager` invoke this
  same composer to author a *new* custom agent on an operator's request. The
  composer exists; the on-request authoring workflow around it does not.

### B3. The four-category agent model

| # | Category | Scope | Deploy target | Selected/created via | Code status today |
|---|---|---|---|---|---|
| 1 | Universal bundled (`research`, `qa`, `version-control`, `documentation`, `code-critic`, `engineer`, `local-ops`, `security`, …) | User | `FrameworkPaths::agent_deploy_dir()` = `<base>/.trusty-tools/trusty-mpm/claude-config/agents` (the relocated `$CLAUDE_CONFIG_DIR`) | Always selected — absent from `LANGUAGE_ENGINEERS` | **LIVE** |
| 2 | Stack-specific bundled (`rust-engineer`, `python-engineer`, `typescript-engineer`, `nextjs-engineer`, …) | Project, **expressed as selection** | Same `agent_deploy_dir()` as category 1 — **not** a separate directory | `[agents] include/exclude` in `manifest.toml`, auto-derived per project by `language_agent_scope` (`core/manifest/project_lang.rs::LANGUAGE_ENGINEERS`) | **LIVE** (selection only; see §Open Questions for the literal-reading tension) |
| 3 | User-installed custom, **project** level | Project | `<project>/.claude/agents/` | `tm-agent-manager`, on request (composing per B2), or Claude Code `/agents` directly | Directory + roster-inclusion **LIVE** (see B4); on-request *creation* via `tm-agent-manager` **NOT built** |
| 4 | User-installed custom, **user** level | User | Managed session: `agent_deploy_dir()` (same dir as category 1). Standalone/non-managed session: real `~/.claude/agents`, which trusty-mpm "never writes to" today | Same as category 3 | Directory + roster-inclusion **LIVE for the managed case** (same dir already scanned); **standalone case is an open question**, see §Open Questions |

Categories 3 and 4 both cover a `/agents`-created agent as well as a
`tm-agent-manager`-built one — trusty-mpm's delegation map must index both, it
does not get to see only its own (§B4). The in-flight PR that deletes tracked
`*/.claude/agents/*.md` and gitignores the directory is **consistent** with
categories 3/4: the directory holds deployed/managed or hand-placed
**artifacts**, never git-tracked **source** — a `/agents`-created project
agent landing there and being covered by that gitignore rule is correct
(it is user-local content, not repository content), not trusty-mpm
suppressing user agents. State this explicitly so a future reader does not
read the gitignore rule backwards.

### B4. Selection vs. deployment — the load-bearing distinction, and the delegation map

`ContentSource` (`schema.rs`) has exactly two variants, `Bundled | Catalog` —
no `Project` variant exists, so `manifest.toml` **cannot** name a project as
the *content source* of a bundled/stack-specific agent; it can only select,
by name, which already-composed bundled agents deploy. Deployment for
categories 1–2 is unconditionally the single `agent_deploy_dir()`
(`session_launch/mod.rs`: `deploy_agents_filtered(&plan.agent_source,
&fw.agent_deploy_dir(), |name| plan.agent_selected(name))`), and any stale
project-local copy is actively retracted every launch
(`retract_framework_agents(&project_dir.join(".claude").join("agents"))`,
issue #4409) — but retraction **only removes framework-owned entries**
("Only manifest-tracked, framework-owned files are removed; hand-placed and
user-owned files are untouched"), which is exactly the seam categories 3/4
rely on.

**The delegation map already unions all relevant tiers today.** THE roster
resolver (`crates/trusty-mpm/src/core/delegation_authority.rs::resolve_roster`,
issue #4588 — "every consumer calls this one function") calls
`deployed_agent_dirs(project_dir)`, which returns, **highest precedence
first**:

1. `<project>/.claude/agents` — its own doc comment: "holds only agents the
   operator hand-placed (and, in future, project-custom trusty-built agents)"
2. the active `CLAUDE_CONFIG_DIR/agents` (env var when set, else
   `managed_claude_config_dir()`) — categories 1/2, and category 4 for a
   managed session
3. `FrameworkPaths::default().claude_agents_dir()` = real `~/.claude/agents`
   — "tm never writes to" this one

`roster_from_dirs` scans every `.md` file in each directory via `scan_agents`
and keeps the first (highest-precedence) summary per name. **This means
categories 3 and 4 (managed case) already reach the delegation map today,
mechanically, for any well-formed `.md` file — regardless of who authored it
or whether it came from `tm-agent-manager` or `/agents`.** The gap is not
roster-inclusion; it is that nothing yet *authors* such a file on request
(B1.2, B2).

### B5. Ownership-ledger and frontmatter reconciliation

Two independent mechanisms exist and answer different questions; a third,
provenance-in-frontmatter, was named in the owner's 2026-08-03 direction and
does **not exist in code today**:

- **The JSON ownership ledger** (`.trusty-mpm-manifest.json` /
  `.trusty-mpm-skills-manifest.json`, `agents::manifest::AgentManifest`)
  answers *"did trusty-mpm write this file, and does it still match what it
  wrote"* — `checksum` + `deployed_at` per managed filename. `Origin` has
  three variants, `Bundled | Registry | User`, and
  `Origin::is_framework_owned()` is `true` only for `Bundled`. **Only
  `Origin::Bundled` entries are ever written by code today** — the deployer
  that populates this ledger is the bundled-agent deployer; nothing writes a
  `Registry` or `User` entry. A file **absent** from the manifest is already
  treated identically to an explicit `User`-origin entry ("not in manifest →
  user-owned → skip silently") — so **absence is already the working signal
  for "not ours, never touch,"** which is exactly the signal PR #4526's
  `is_movable` predicate (issue #4448) needs and currently lacks by name.
- **Frontmatter** (`crates/trusty-agents-common/src/agents/frontmatter.rs`,
  parsed by the generic `parse_kv_line`) answers *"what IS this file."*
  Enumerated exhaustively across every bundled agent and every `BASE-*`
  fragment (`crates/trusty-mpm/src/assets/agents/*.md`), the complete set of
  frontmatter keys in use today is: `name`, `role`, `extends`, `description`,
  `model`, `skills`. **No `origin`/`provenance` field exists in any bundled
  agent today, and the parser defines no such key.** The owner's 2026-08-03
  statement ("We use frontmatter to track this") is therefore a **target
  design, not a description of shipped code** — flagged here rather than
  written up as though it already works.
- These two mechanisms overlap in *what they could* express (both could say
  "trusty-mpm made this") but today only the ledger says anything at all, and
  only for the one category (`Bundled`) it already handles. Whether a
  frontmatter provenance field, once added, would need to agree with the
  ledger, and which wins if they disagree, is unwritten in code and is
  recorded as an open question below rather than invented here.

### B6. What happens when the manifest names something absent from the roster

- **A `[agents] include` entry naming a bundled/stack-specific agent stem not
  present in the compiled roster:** whether this fails loudly today is
  **UNVERIFIED** — ADR-0025's own text above states plainly that no such
  check was found in the delegation path (clause 12 exists to close this gap
  and has not shipped). Not independently re-verified in this pass; carried
  forward as-is.
- **A manifest referencing a category-3/4 custom agent file that was never
  created:** `resolve_roster` simply will not find it; no error is surfaced
  anywhere today. This is a plain consequence of B4, not a new finding.

## Open Questions (2026-08-03 addendum — named, not resolved)

1. **Clause 1 vs. categories 3/4.** Clause 1 states "A project cannot ship
   its own agent FILE — it can only select, by name, from the catalog…
   Per-project canonical agent directories… are explicitly deferred to a
   follow-on ADR if they prove necessary." Category 3 (and the managed case
   of category 4) is precisely a project-level agent FILE, and `resolve_roster`
   already gives it the HIGHEST precedence tier. Read charitably, `deployed_agent_dirs`'s
   own "(and, in future, project-custom trusty-built agents)" comment is the
   same deferred work clause 1 names — but ADR-0025 has not been amended to
   say this addendum IS that follow-on trigger. Flagged, not resolved.
2. **Clause 14 vs. the in-flight tracked-file deletion.** Clause 14 requires
   migration to be report-first, copy-then-quarantine, never deletion. The
   in-flight PR deleting the ~34 tracked `*/.claude/agents/*.md` files (stale,
   superseded pre-#4409 per-crate artifacts) plus a gitignore rule may or may
   not count as "migration" under clause 14's definition — if it does, it
   needs the quarantine treatment; if it is ordinary dead-file cleanup of
   already-superseded tracked content, it does not. Not resolved here.
3. **Unresolvable delegation target.** Whether an unresolvable agent name
   fails loudly today is UNVERIFIED (see B6 and ADR-0025's own Consequences
   section above).
4. **Category 4, standalone/non-managed session.** A user-level custom agent
   must land in whichever directory the ACTIVE harness resolves as its user
   tier for that session type. For a managed session that is the relocated
   `$CLAUDE_CONFIG_DIR` (`agent_deploy_dir()`), already scanned. For a
   standalone `tm` session (no daemon orchestration), `deployed_agent_dirs`'s
   third tier is the real `~/.claude/agents`, which trusty-mpm's own code
   comment says it "never writes to." Whether that is a defect for category 4
   (the harness would not see a user-level custom agent created for a
   standalone session) or is out of scope for a session type this addendum
   does not otherwise address is not resolved here.
5. **Ledger vs. frontmatter precedence.** If a future frontmatter provenance
   field and the JSON ownership ledger ever disagree about a file's origin,
   which wins is unwritten in code today (B5) and is not invented here.
6. **Relationship to issue #4443** ("custom deployment mechanism for
   trusty-built project agents," open, milestone 1.3.4). #4443 was scoped
   before the 2026-08-01 tier-collapse ruling under a frontmatter-declared-tier
   framing that ruling superseded. The owner's 2026-08-03 "we use frontmatter
   to track this" may be about PROVENANCE (this addendum's B5), not the
   superseded TIER-declaration approach — these may not be the same claim.
   Not resolved here; #4443 should be read against this addendum before
   further scoping.

## Deliberately Out of Scope (this addendum)

Carried forward from the loose-spec instruction — named so the omissions are
visible rather than silent:

- A numeric multi-source priority list (`agents-sources.toml`, clause 3).
- An `Origin::Project` (or any fourth `Origin`) variant, or a `ContentSource::Project`
  variant — categories 3/4 are expressed as directories the roster already
  scans, not as a manifest content-source change.
- Startup reconciliation (clause 9) and the loud-failure check (clause 12) —
  both explicitly gated behind their own critic passes in the Slices section
  above; untouched by this addendum.
- A skill-side equivalent of `language_agent_scope`/`LANGUAGE_ENGINEERS` — no
  such auto-detection exists for skills today (confirmed: `grep` across
  `trusty-agents-common/src/skills/tiers.rs` for language/stack/marker logic
  returns nothing); `[skills] include/exclude` stays manual-only selection in
  this pass. The 33-of-103 `vercel:*`-skills-in-a-non-Vercel-repo case
  (issue #4528's own motivating measurement) is not solved here.
- A new frontmatter provenance field, or code that writes one — B5 names the
  gap, it does not close it.
- `docs/adr/INDEX.md`'s one-line summary for ADR-0025 — left unedited to keep
  this addendum's diff minimal; its content is still accurate as a summary of
  the original clauses, and this addendum is additive.

## Related Decisions

Vetted against prior ADRs on 2026-08-01:

- **ADR-0002 (single-install convention):** Consistent — extends the same instinct from binaries to assets.
- **ADR-0008 (project identity):** Extends — must not introduce a second way to name a project.
- **ADR-0015 (three-product agent composition):** Consistent — ADR-0015 governs FORMAT, this governs SOURCING and DEPLOYMENT for trusty-mpm only.
- **ADR-0020 (session-owned worktrees):** Consistent, and a deliberate borrowing — its "owner-unknown candidates are NEVER auto-deleted" is the same rule as clause 14. If they ever diverge, ADR-0020's formulation is senior.
- **ADR-0023 (worktree authority):** Consistent, with one gap stated openly — ADR-0023 clause 4 requires the ownership index be REBUILDABLE from ground truth. Neither the agent nor the skill ledger is rebuildable today; a lost manifest permanently reclassifies every managed file as untracked, which is #4408's shape reached by data loss. This ADR does not close that gap and does not claim to; filed as follow-up.
- **ADR-0024 (assistants as L0 delegators):** Consistent, orthogonal axis. Caution: its clause-4 editable sub-agent whitelist must consume this ADR's resolved roster rather than re-deriving one.
- **Skills have no prior ADR** — this is the first.
- **DOC-31 (`docs/specs/system-project-agents-skills.md`):** Superseded in part — §191 names committed project-tier agents in `.claude/agents/` as supported, which clause 1 removes. DOC-31 must be rewritten in the change set that lands that clause.
- Note (resolved by the ADR consistency repair): the former Cargo-policy half of
  the ADR-0021 collision is now [ADR-0043](0043-cargo-bin-policy.md); ADR-0021
  remains the Slack inbound decision.
