# DOC-31 — SYSTEM vs PROJECT Agents & Skills — Provisioning, In-Project Migration, Requirement-Driven Pulls

**Status:** Draft
**Subsystem:** trusty-mpm — content provisioning / agent + skill deploy pipeline
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-07-02
**Spec ID:** `SPEC-PROVISION-01~draft` … `SPEC-PROVISION-08~draft` (DOC-31)
**Linked issues:** [#1928](https://github.com/bobmatnyc/trusty-tools/issues/1928) (this spec); [#1927](https://github.com/bobmatnyc/trusty-tools/issues/1927) (**dependency** — the DOC-24 §SPEC-STANDALONE-MPM-04 drift bug this spec's isolation contract depends on; must be fixed first).
**Builds on:** DOC-24 — Standalone Managed `trusty-mpm` Driver (`docs/specs/standalone-managed-trusty-mpm.md`, the project-local `repo/.claude/` model and the isolation invariant that tm never writes the user's real `~/.claude*`); DOC-17 — Autonomous Multi-Session Managed Harness Runner (`docs/specs/harness-runner-vision.md`, HR-2 manifest-driven provisioning precedence + catalog sync); DOC-29 — Primary trusty-mpm Harness Behaviors (`docs/specs/mpm-behavior-conformance.md`, BHV-04 agent/skill bundling + BHV-06 catalog sync, the conformance-row house style §2 adopts).
**Cross-ref:** the converged deploy hot path (`crates/trusty-mpm/src/core/session_launch/mod.rs`, `prepare_session_inner` @ line 238; `deploy_agents_filtered` → `fw.claude_agents_dir()` @ line 279; `deploy_skills_filtered` → `fw.claude_skills_dir()` @ line 307); the agent/skill deployers + ownership ledgers (`crates/trusty-mpm/src/core/agent_deployer.rs`, `deploy_agents_filtered` @ lines 92–192; `crates/trusty-mpm/src/core/agent_manifest.rs`, `MANIFEST_FILE` @ line 22, `Origin` @ lines 87–94; `crates/trusty-mpm/src/core/skill_deployer.rs`, `deploy_skills_filtered` @ line 92; `crates/trusty-mpm/src/core/skill_manifest.rs`); manifest layering (`crates/trusty-mpm/src/core/manifest/schema.rs`, `ContentSource` @ lines 126–134; `crates/trusty-mpm/src/core/manifest/resolve.rs`, `resolve_manifest` @ lines 95–116); the catalog sync machinery (`crates/trusty-mpm/src/content/catalog_sync.rs`, `catalog_root_for` @ line 39, `DEFAULT_CATALOG_REPO` @ line 43); the spawn flows (`crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs`, `spawn_managed_cloned` @ line 224; `crates/trusty-mpm/src/daemon/managed_routes/inproject.rs`, base clone + `.worktrees/`; `crates/trusty-mpm/src/core/standalone/load.rs`, `run_prepare_session` @ line 176 — the #1927 drift site); project scaffold (`crates/trusty-mpm/src/bin/tm/commands/project.rs`, `scaffold_project_dir` @ line 105); framework-path derivation (`crates/trusty-mpm/src/core/paths.rs`, `agent_source_dir` / `skill_source_dir` @ lines 231–258); the provisioning-stage emitter (`crate::core::provisioning_stage`).

> **Scope note.** This is a **behavior contract** for how trusty-mpm classifies, sources,
> deploys, and migrates **agents and skills** across two ownership tiers — **SYSTEM** (framework-
> managed, manifest-tracked) and **PROJECT** (project-owned, natively discovered) — plus two new
> features: **F1**, migrating a user project's pre-existing non-platform agents/skills into tm's
> projected source tree as PROJECT content; and **F2**, requirement-driven scaffolding pulls that
> select a language/framework-appropriate subset of catalog agents/skills for a **new** project.
> It sits **on top of** the already-merged deploy pipeline (§1) and DOC-24's project-local
> `repo/.claude/` model, and **depends on** the DOC-24 isolation invariant (§SPEC-STANDALONE-MPM-04)
> holding — which the #1927 fix restores (§Out-of-scope). This spec specifies the **what**; the
> implementing modules are the **where**; the **how** is left to the WI plan (Appendix A). It does
> **not** re-spec the session lifecycle, the provisioner clone mechanics, the instruction pipeline,
> or the standalone-driver command surface — those are consumed as-is.

---

## Purpose & Scope

trusty-mpm today deploys a single, **global**, framework-bundled catalog of agents and skills to
`~/.claude/agents/` and `~/.claude/skills/` on every session prep (`prepare_session_inner`,
`session_launch/mod.rs:238`). There is **no** notion of *project-owned* content: a user project that
already carries its own `.claude/agents/*.md` or `.claude/skills/*/SKILL.md` (authored by the team,
not by tm) gets those files only incidentally — because a full clone of the repo happens to contain
the committed copies — and tm records **nothing** about them, so a deploy can shadow or (in the
degenerate global case) collide with them, and non-in-project (managed-clone) sessions of the same
project never see uncommitted project content at all.

This spec introduces a **two-tier taxonomy** — SYSTEM vs PROJECT — with an explicit precedence model,
extends the ownership ledger and manifest schema to represent project-owned content as a first-class
source, and specifies the two features that populate the PROJECT tier: **F1** (migrate a project's
existing non-platform content) and **F2** (pull a requirement-appropriate catalog subset into a new
project). Throughout, it honors DOC-24's **segregation invariant**: tm writes PROJECT content **only**
into tm-owned workspaces, **never** into the user's live checkout or the real `~/.claude`.

**In scope:** the SYSTEM/PROJECT/USER-GLOBAL taxonomy and precedence (project > user > system);
ownership/origin ledger extensions (`ContentSource::Project`, an `Origin` producer for project content,
a per-project manifest); F1 in-project migration behavior (detection, read-only extraction, placement,
collision/shadow handling, idempotency, uncommitted-content handling); F2 requirement-driven selection
(requirement detection, selection manifest, catalog extension, the tm-agent-manager skill contract);
the segregation invariants; observability (provisioning stages + shadow/migration logs); and a
conformance matrix mapping each requirement to the code path that will satisfy it.

**Out of scope** (consumed, not re-specified): the DOC-24 command lexicon and `CLAUDE_CONFIG_DIR`
isolation primitive; the session lifecycle / `SessionRecord` (DOC-14); the provisioner clone mechanics;
the instruction pipeline and agent compose chain (DOC-17/DOC-29 BHV-01/04); the autonomy tiers
(DOC-23). The **#1927** DOC-24 drift fix is a **dependency**, not a deliverable here (§Out-of-scope).

---

## Terminology

Three ownership tiers, named consistently across the whole document:

| Tier | Definition | Where it lives | Who owns / writes it |
|---|---|---|---|
| **SYSTEM** | Framework-managed agents/skills: the compile-time **bundled** assets (`assets/{agents,skills}` via `core::bundle`, or the `agents/` submodule when present, `paths.rs:231–258`) and the **catalog**-synced set (`~/.trusty-mpm/catalog/repo/.claude/…`, `catalog_sync.rs`). Manifest-tracked (`.trusty-mpm-manifest.json`), updatable, checksum-classified. | The deploy **target** (`~/.claude/{agents,skills}` today; `repo/.claude/{agents,skills}` under DOC-24). | tm owns and may overwrite (subject to user-modified preservation). |
| **PROJECT** | Project-owned agents/skills: content authored by the **project team** (or pulled in for the project by F2), discovered **natively** by Claude Code from the checkout's `.claude/`. Recorded as `ContentSource::Project` / `Origin::Project` so deploys **never clobber** it. | Inside a **tm-owned workspace** only: `<workspace>/.claude/{agents,skills}` (base clone / worktree / managed clone). | The project team owns the *content*; tm owns the *placement/ledger* in its workspace. |
| **USER-GLOBAL** | The user's **real** `~/.claude` (and `~/.claude.json`): the maintainer's own claude-mpm agents/skills/hooks/MCPs. **Out of bounds for tm writes** per the DOC-24 segregation directive. | The real `~/.claude` on the operator's machine. | The user. tm **reads never, writes never** (2026-07-02 segregation directive). |

**Platform vs non-platform content.** Within a project checkout, an agent/skill file is **platform
content** (SYSTEM, tm-manifest-managed) iff it appears in the deploy target's `.trusty-mpm-manifest.json`
(agents) / skill manifest with a matching origin; it is **non-platform content** (a PROJECT-migration
candidate) otherwise — i.e. any `.claude/agents/*.md` or `.claude/skills/*/SKILL.md` that tm did not
deploy. This is exactly the "not-in-manifest ⇒ user-owned" classification the deployers already make
(`agent_deployer.rs:145`), lifted to drive F1.

**Shadow.** When a PROJECT-tier file and a SYSTEM-tier file share a name, PROJECT wins and SYSTEM is
**shadowed** (present-but-inactive). A shadow is a normal, expected outcome — it is **logged and
observable** (§SPEC-PROVISION-07), never silent.

---

## Table of Contents

| ID | Section | Implementing module(s) |
|----|---------|--------------------------|
| SPEC-PROVISION-01~draft | [Taxonomy & precedence model](#taxonomy--precedence-model-spec-provision-01draft) | `core::manifest`, `core::session_launch` |
| SPEC-PROVISION-02~draft | [Project-level content home (segregated, tm-owned)](#project-level-content-home-spec-provision-02draft) | `core::managed_config`, `daemon::managed_routes::inproject` |
| SPEC-PROVISION-03~draft | [Ownership & origin ledger extensions](#ownership--origin-ledger-extensions-spec-provision-03draft) | `core::manifest::schema`, `core::agent_manifest`, `core::skill_manifest` |
| SPEC-PROVISION-04~draft | [F1 — In-project migration of non-platform content](#f1--in-project-migration-of-non-platform-content-spec-provision-04draft) | `daemon::managed_routes::inproject`, `core::project_migrate` (new) |
| SPEC-PROVISION-05~draft | [F2 — Requirement-driven scaffolding pulls](#f2--requirement-driven-scaffolding-pulls-spec-provision-05draft) | `content::catalog_sync`, `core::requirement_detect` (new), tm-agent-manager skill |
| SPEC-PROVISION-06~draft | [Segregation invariants](#segregation-invariants-spec-provision-06draft) | `core::agent_deployer`, `core::skill_deployer`, `core::project_migrate` (new) |
| SPEC-PROVISION-07~draft | [Observability — provisioning stages, shadow & migration logs](#observability-spec-provision-07draft) | `core::provisioning_stage`, deployers, `core::project_migrate` (new) |
| SPEC-PROVISION-08~draft | [Conformance matrix](#conformance-matrix-spec-provision-08draft) | cross-cutting |

---

## 1. Motivation & Current State (verified 2026-07-02)

### 1.1 The converged deploy path is SYSTEM-only, GLOBAL-targeted

Every spawn path — managed clone (`spawn_managed_cloned`, `lifecycle.rs:224`), in-project
(`inproject.rs`), local-path fallback (redirects to the clone flow), and the standalone driver
(`standalone/load.rs:176`) — converges on `prepare_session_inner` (`session_launch/mod.rs:238`). There
it deploys the manifest-selected SYSTEM catalog:

| # | Step | Target (today) | Call site |
|---|------|----------------|-----------|
| 1 | Deploy composed agents | `~/.claude/agents/` (`fw.claude_agents_dir()`) | `deploy_agents_filtered` (`mod.rs:279`) |
| 2 | Deploy skills | `~/.claude/skills/` (`fw.claude_skills_dir()`) | `deploy_skills_filtered` (`mod.rs:307`) |

Both targets are **global**. **Per-project content does not exist** as a concept: there is no
PROJECT tier, no per-project source, and no migration of a project's own `.claude/` content.

### 1.2 Manifest layering selects, it does not carry PROJECT content

The HR-2 manifest resolver (`resolve_manifest`, `resolve.rs:95–116`) overlays four layers
lowest-to-highest — **compiled default → catalog → user → project** — but this governs *selection*
(which named agents/skills deploy, from which `ContentSource`), **not content provenance**. The
`ContentSource` enum (`schema.rs:126–134`) has exactly two variants today — `Bundled | Catalog` —
both SYSTEM. There is no way to say "this content is project-owned."

### 1.3 The ownership ledger already classifies — but only into SYSTEM vs user-dropped

Each deploy target carries `.trusty-mpm-manifest.json` (`agent_manifest.rs:22`; skills have the exact
parallel in `skill_manifest.rs`). `deploy_agents_filtered` (`agent_deployer.rs:92–192`) classifies each
existing target file:

- **not-in-manifest ⇒ user-owned** → skipped, never touched (`agent_deployer.rs:145–148`);
- **in-manifest + checksum-match ⇒ managed** → safe to refresh (`:151`);
- **in-manifest + checksum-differs ⇒ user-modified** → preserved (`:158–162`).

The `Origin` enum (`agent_manifest.rs:87–94`) already has `Bundled | Registry | User`, but **every**
write hardcodes `Origin::Bundled` (`agent_deployer.rs:180`) — nothing produces `Origin::User`, and there
is no `Origin::Project`. So the *shape* for representing project content is present but **unused**.

### 1.4 In-project already clones committed `.claude/`, but records nothing about it

The in-project path (`inproject.rs`) makes a durable **base clone** at
`<repos_root>/<owner>/<repo>/` (`DEFAULT_REPOS_DIR = "trusty-mpm-projects"`, `inproject.rs:41`) and
per-session **worktrees** under `<base>/.worktrees/<session_id>` (`workspace_owned = false`). The base
clone already contains the repo's **committed** `.claude/{agents,skills}`. So F1 migration matters
mainly for: (a) **uncommitted / local-only** content in the operator's live checkout that the clone
does not have; (b) **recording origin/ownership** so a SYSTEM deploy never clobbers project content;
(c) making project content **deployable to non-in-project (managed-clone) sessions** of the same
project, where no committed `.claude/` is guaranteed to match the live checkout.

### 1.5 New-project scaffold deploys no content

`tm project init` (`scaffold_project_dir`, `project.rs:105`) writes only `.trusty-mpm/{sessions/,
config.toml}` — it never deploys agents or skills. F2 gives scaffolding a content-pull step.

### 1.6 Catalog source (F2 substrate)

`CatalogSync` (`catalog_sync.rs`) fetches `DEFAULT_CATALOG_REPO = bobmatnyc/claude-mpm` (`:43`, ref
`main`) into `~/.trusty-mpm/catalog/repo/.claude/{agents,skills}` (`catalog_root_for`, `:39`) with a
24h-TTL, **all-or-nothing** sync; `/mpm-*` skills are protected (`core::stale_skills`). F2 extends this
with requirement-driven **selection on top** — it does **not** introduce a second cache.

---

## 2. Behavior Contract Sections

### Taxonomy & precedence model {#SPEC-PROVISION-01~draft}

**ID:** SPEC-PROVISION-01~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** a resolved session workspace (base clone / worktree / managed clone), the SYSTEM
  content sources (bundled + catalog), and any PROJECT content present in the workspace's `.claude/`.
- **Outputs:** an **effective** agent/skill set the launched session sees, computed by layering
  PROJECT over SYSTEM by name. The precedence is, highest to lowest:
  **PROJECT (`repo/.claude/…`) > USER (the tm-managed user layer) > SYSTEM (bundled/catalog deploy)** —
  the same layering Claude Code itself applies (project > user > system). USER-GLOBAL (the real
  `~/.claude`) is **excluded** from a tm session entirely (DOC-24 `CLAUDE_CONFIG_DIR`); it is never a
  precedence participant.
- **Preconditions:** the deploy target and PROJECT content home are both inside a tm-owned workspace
  (§SPEC-PROVISION-02); the #1927 fix holds so SYSTEM deploy does **not** land in the real `~/.claude`.
- **Postconditions:** for any name present in both tiers, the PROJECT file is the one the session
  resolves; the shadowed SYSTEM file remains on disk (updatable by SYSTEM refresh) but inactive; every
  such shadow is recorded (§SPEC-PROVISION-07). Names present in only one tier resolve to that tier.
- **Error conditions:** an ambiguous case where two sources claim the **same tier** for the same name
  (e.g. bundled and catalog both selected for one agent) is resolved by the existing manifest
  `ContentSource` selection (`schema.rs`), not by this precedence rule; PROJECT-vs-SYSTEM is the only
  cross-tier rule this ID governs.

#### Rationale (WHY)

A single flat global catalog cannot express "this project brings its own reviewer agent that must win
over the bundled one." Mirroring Claude Code's native **project > user > system** layering makes the tm
model predictable to anyone who already understands Claude Code, and makes PROJECT content authoritative
for the project that owns it without deleting the SYSTEM fallback (so a project can drop its override and
transparently fall back). Making shadows observable rather than silent is what keeps "why did my agent
change?" answerable.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::manifest` (resolve/schema) | Carries the tier of each selected item (extended in §03); selection within a tier. |
| `core::session_launch` | Applies PROJECT-over-SYSTEM name layering at deploy time; emits shadow records. |

---

### Project-level content home (segregated, tm-owned) {#SPEC-PROVISION-02~draft}

**ID:** SPEC-PROVISION-02~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** the resolved workspace path for a session (managed clone dir; in-project base clone
  `<repos_root>/<owner>/<repo>/`; or a per-session worktree `<base>/.worktrees/<session_id>`).
- **Outputs:** PROJECT agents/skills live at **`<workspace>/.claude/{agents,skills}`** — the checkout's
  own `.claude/`, which Claude Code discovers natively (matching DOC-24 §SPEC-STANDALONE-MPM-03's
  project-local model). This is the **only** place tm writes or relocates PROJECT content.
- **Preconditions:** the workspace is **tm-owned** — a base clone, a worktree, or a managed clone that
  tm provisioned. A path that is the operator's **live/primary checkout** is **not** tm-owned and is
  **never** a write target (it is a read-only migration *source* only, §SPEC-PROVISION-04).
- **Postconditions:** PROJECT content is present and discoverable in **every** tm-managed session for
  that project (both in-project worktrees and managed-clone sessions); no PROJECT write ever touches the
  user's live checkout or the real `~/.claude` (§SPEC-PROVISION-06).
- **Error conditions:** if the resolved workspace cannot be confirmed tm-owned, the operation **fails
  closed** (no write) rather than risk writing the user's live tree.

#### Rationale (WHY)

The 2026-07-02 **segregation directive** was issued after a confirmed cross-tool pollution incident: a
shared global `~/.claude/agents` let one tool's agents leak into another's sessions. Homing PROJECT
content in the **checkout's own `.claude/`** inside a **tm-owned** workspace gives native discovery (no
flags, IDE-honored — DOC-24 §06) while keeping every write inside a tree tm controls. Reusing DOC-24's
`repo/.claude/` model means F1/F2 land content exactly where the standalone driver already deploys
project-local SYSTEM agents, so one placement rule serves both. The live-checkout-is-read-only rule is
the concrete mechanism that prevents a repeat of the pollution incident.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::managed_config` (DOC-24) | Owns the `repo/.claude/{agents,skills}` layout; the shared placement definition. |
| `daemon::managed_routes::inproject` | Resolves the tm-owned base clone / worktree; asserts tm-ownership before any PROJECT write. |

---

### Ownership & origin ledger extensions {#SPEC-PROVISION-03~draft}

**ID:** SPEC-PROVISION-03~draft
**Status:** Draft

#### Behavior Contract (WHAT)

Three coordinated extensions give PROJECT content a first-class representation:

1. **`ContentSource::Project`** — a new variant on the manifest `ContentSource` enum
   (`schema.rs:126–134`, today `Bundled | Catalog`). It denotes "source this item from the project's
   own `.claude/`," distinct from the two SYSTEM sources. `Default` stays `Bundled` (zero-regression).
2. **An `Origin` producer for PROJECT content** — the `Origin` enum (`agent_manifest.rs:87–94`, today
   `Bundled | Registry | User`, **all writes hardcode `Bundled`**, `agent_deployer.rs:180`) gains a
   producer: F1-migrated and F2-pulled project content is recorded with an origin distinct from
   `Bundled`. This spec introduces **`Origin::Project`** (and gives the dormant `Origin::User` a
   producer for genuinely user-dropped files) so the ledger can tell PROJECT content apart from
   framework content and from user-dropped files.
3. **A per-project manifest** — PROJECT content in `<workspace>/.claude/{agents,skills}` is tracked by
   the **same** `.trusty-mpm-manifest.json` ledger the deployers already write (`agent_manifest.rs:22`,
   skill parallel), with PROJECT entries carrying `Origin::Project` / `ContentSource::Project`. This
   makes the "is this file safe to overwrite?" decision (`agent_deployer.rs:143–162`) tier-aware:
   PROJECT-origin entries are **never** overwritten by a SYSTEM deploy.

- **Inputs:** a deploy/migration operation producing an agent/skill file.
- **Outputs:** a manifest entry tagged with the correct `Origin` and (for selection) `ContentSource`.
- **Preconditions:** the target is a tm-owned workspace (§02).
- **Postconditions:** a subsequent SYSTEM `deploy_agents_filtered` / `deploy_skills_filtered` classifies
  a PROJECT-origin file as **not framework-managed → preserve** (extending the existing not-in-manifest
  skip at `agent_deployer.rs:145` to also cover in-manifest-but-`Origin::Project` entries); a SYSTEM
  file of the same name that would shadow it is recorded as shadowed (§07).
- **Error conditions:** a corrupt manifest still surfaces as the existing hard error
  (`agent_deployer.rs:110–115`), never a silent reclassification.

#### Rationale (WHY)

The deployers already make the exact ownership decision F1/F2 need — "did tm write this, and if so is it
unmodified?" — but only in a binary SYSTEM-vs-unknown sense, and every entry is stamped `Bundled`. Adding
`ContentSource::Project` + an `Origin::Project` producer promotes the *already-present-but-unused* shape
(`Origin::User`/`Registry` exist yet nothing writes them) into a real three-way distinction, so a SYSTEM
refresh can safely skip PROJECT files by reading the ledger instead of by filename heuristics. Reusing the
**same** manifest file rather than inventing a parallel PROJECT ledger keeps one source of truth per
directory and one corruption/atomic-write path (`atomic_write`, `agent_manifest.rs:51`).

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::manifest::schema` | Add `ContentSource::Project` (default unchanged). |
| `core::agent_manifest` / `core::skill_manifest` | Add the `Origin::Project` producer; keep the single ledger + atomic-write/corruption path. |
| `core::agent_deployer` / `core::skill_deployer` | Make the classify step (`:143–162`) tier-aware: never overwrite `Origin::Project`; stop hardcoding `Origin::Bundled` where a producer applies. |

---

### F1 — In-project migration of non-platform content {#SPEC-PROVISION-04~draft}

**ID:** SPEC-PROVISION-04~draft
**Status:** Draft

#### Behavior Contract (WHAT)

When tm runs against an **existing** user project (the in-project path, `inproject.rs`), it migrates the
project's **non-platform** agents/skills into tm's projected source tree as PROJECT content.

- **Inputs:** the operator's **live checkout** (the cwd from which `tm` was invoked) as a **read-only
  source**, and the tm-owned base clone `<repos_root>/<owner>/<repo>/` (and its worktrees) as the write
  target.
- **Detection & classification:** enumerate `.claude/agents/*.md` and `.claude/skills/*/SKILL.md` in the
  live checkout. Classify each as **platform** (SYSTEM — present in the deploy target's
  `.trusty-mpm-manifest.json` with a framework origin) or **non-platform** (a migration candidate — the
  "not-in-manifest ⇒ user-owned" test, `agent_deployer.rs:145`, applied to the source). Only
  **non-platform** files are migrated.
- **Read-only extraction & placement:** copy each non-platform file **from** the live checkout **into**
  `<workspace>/.claude/{agents,skills}` in the tm-owned tree, recording each with `Origin::Project` /
  `ContentSource::Project` (§03). The live checkout is **never written** (§06).
- **Collision / shadow handling:** if a migrated PROJECT file's name collides with a SYSTEM file, PROJECT
  wins (§01) and the SYSTEM file is recorded as shadowed (§07). If it collides with a **committed** copy
  already in the base clone, the migrated (live-checkout) version — which may carry uncommitted edits —
  takes precedence, and the divergence is logged.
- **Uncommitted-content handling:** the primary value of F1 is exactly the **uncommitted / local-only**
  non-platform content the base clone does not contain (§1.4). Such files are migrated the same way;
  because the source is read-only, no uncommitted work is ever mutated or committed on the user's behalf.
- **Idempotency / re-run semantics:** migration is keyed by `(name, source-checksum)` in the per-project
  manifest. A re-run re-copies a source file only when its checksum changed since the last migration;
  an unchanged file is a no-op (`unchanged`), a changed source file refreshes the PROJECT copy **unless**
  the PROJECT copy was edited in the workspace since migration (checksum-diff ⇒ preserve, mirroring
  `agent_deployer.rs:158`). A file deleted from the source is **left in place** in the workspace
  (deselection does not delete, matching the deployer's HR-3 stance, `agent_deployer.rs:88`) but flagged.
- **Preconditions:** the invocation is the in-project path; the base clone (write target) is confirmed
  tm-owned (§02); the live checkout is resolvable and readable.
- **Postconditions:** every non-platform agent/skill from the live checkout is present as PROJECT content
  in the tm-owned workspace, discoverable in every tm-managed session for that project (worktrees **and**
  managed-clone sessions), ledgered with `Origin::Project`, and never clobbered by a SYSTEM refresh.
- **Error conditions:** an unreadable source file is skipped with a warning (migration continues — one
  bad file must not abort the session); a write that would land outside the tm-owned tree **fails closed**
  (§06); a corrupt per-project manifest surfaces the existing hard error, not a silent reset.

#### Rationale (WHY)

A team that already invested in `.claude/agents/reviewer.md` expects tm sessions to use *their* reviewer,
not to have it silently ignored or (worse) overwritten. Because the base clone already carries
**committed** project content (§1.4), F1's real job is three-fold: capture **uncommitted** local content
the clone lacks, **record ownership** so SYSTEM deploys preserve project content instead of treating it as
fair game, and **propagate** project content to managed-clone sessions that don't share the live checkout.
Doing this as a **read-only** extraction from the live checkout is the direct, mechanical enforcement of
the segregation directive: the one tree tm must never mutate (the user's working copy) is only ever read.
Checksum-keyed idempotency lets `run`/spawn call migration unconditionally without re-copying or
clobbering edited project files, exactly as the deployers already do for SYSTEM content.

#### Implementing Modules

| Module | Role |
|--------|------|
| `daemon::managed_routes::inproject` | Resolves live-checkout source + tm-owned target; invokes migration in the in-project spawn. |
| `core::project_migrate` (new) | Enumerate → classify (platform vs non-platform) → read-only copy → ledger `Origin::Project`; idempotency + shadow records. |
| `core::agent_manifest` / `core::skill_manifest` | Per-project ledger entries (`Origin::Project`, source-checksum) driving idempotent re-run. |

---

### F2 — Requirement-driven scaffolding pulls {#SPEC-PROVISION-05~draft}

**ID:** SPEC-PROVISION-05~draft
**Status:** Draft

#### Behavior Contract (WHAT)

When scaffolding a **new** project, the **tm-agent-manager** skill can pull a **requirement-appropriate
subset** of agents/skills from the catalog sources, landing them as PROJECT content.

- **Inputs:** a new/target project directory (e.g. from `tm project init`, `project.rs:105`) and its
  detected **requirements** — language/framework/stack signals (e.g. `Cargo.toml` → Rust, `package.json`
  + framework dep → Node/Next, `pyproject.toml` → Python, `go.mod` → Go).
- **Requirement detection:** a detector inspects manifest/marker files in the project to produce a set of
  requirement tags. Detection is best-effort and additive: unknown stacks yield an empty tag set (⇒ no
  pull, never an error).
- **Selection manifest:** requirement tags map to a **selection** of catalog agent/skill names (e.g.
  Rust ⇒ `rust-engineer` + relevant skills). The selection is a declarative mapping (a **selection
  manifest**), not hardcoded logic, so the tag→content map is data-driven and reviewable. Selection is a
  **subset** of the catalog — never an all-or-nothing pull.
- **Catalog extension (NOT a new cache):** the pull sources from the **existing** `catalog_sync`
  machinery — `~/.trusty-mpm/catalog` populated from `bobmatnyc/claude-mpm` (`catalog_sync.rs:39,43`),
  24h-TTL, `/mpm-*`-protected. F2 adds **requirement-driven selection on top** of that checkout; it does
  **not** introduce a second catalog concept or cache. If the catalog checkout is stale/absent, the pull
  either uses what is on disk or triggers the existing sync — it never invents a parallel fetch path.
- **Placement:** selected content is copied into `<workspace>/.claude/{agents,skills}` as PROJECT content
  (`Origin::Project` / `ContentSource::Project`, §03) — identical placement to F1, so a scaffolded project
  and a migrated one converge on the same PROJECT home and precedence (§01/§02).
- **tm-agent-manager skill contract:** the skill (a) invokes requirement detection, (b) proposes the
  selected subset to the user, (c) on confirmation performs the catalog-sourced pull via `project_migrate`
  placement, and (d) reports what was pulled and any shadows. The skill is the *driver*; the deterministic
  copy/ledger/segregation logic lives in the same `core::project_migrate` module F1 uses.
- **Preconditions:** the target is a tm-owned workspace (§02); the catalog checkout is available (or
  syncable via the existing path).
- **Postconditions:** a requirement-appropriate PROJECT subset is present and discoverable; the pull is
  ledgered so a later SYSTEM deploy never clobbers it and so re-running the pull is idempotent (§04 rules).
- **Error conditions:** no detected requirements ⇒ no-op (not an error); a requested catalog item absent
  from the checkout ⇒ skip-with-warning (never fabricate content); write outside the tm-owned tree ⇒
  fail closed (§06).

#### Rationale (WHY)

Scaffolding a fresh project with the **entire** catalog is noise — a Rust service does not want the
Next.js agent. Detecting the stack and pulling only the relevant subset makes a new tm project useful on
first launch without manual curation. Driving the tag→content mapping from a **selection manifest** keeps
the policy data-driven and reviewable rather than buried in imperative code. Sourcing from the **existing**
`catalog_sync` checkout (rather than a new cache) is a deliberate constraint: the catalog already has a
TTL, a protected-skills rule, and an all-or-nothing sync — F2 only adds a *selection* filter, so there is
one catalog, one TTL, one protection rule. Landing pulls at the same PROJECT home as F1 means precedence,
segregation, and ledger rules are specified once and reused.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::requirement_detect` (new) | Inspect project marker files → requirement tags (best-effort, additive). |
| selection manifest (data) | Declarative requirement-tag → catalog-name map; reviewable, not hardcoded. |
| `content::catalog_sync` | The single catalog checkout F2 selects from (extended with selection, not a new cache). |
| `core::project_migrate` (new) | Shared copy/ledger/segregation placement (same as F1). |
| tm-agent-manager skill | Drives detect → propose → confirm → pull → report; deterministic logic delegated to `project_migrate`. |

---

### Segregation invariants {#SPEC-PROVISION-06~draft}

**ID:** SPEC-PROVISION-06~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** any F1/F2 migration or SYSTEM/PROJECT deploy operation.
- **Outputs — the invariants:**
  1. **Never write the user's live checkout.** F1 reads the operator's live checkout as a **read-only**
     source; no F1/F2 operation creates, modifies, or deletes any file under the live checkout — not its
     `.claude/`, not its source, not its git state.
  2. **Never write the real `~/.claude` / `~/.claude.json`.** All PROJECT (and, under DOC-24, SYSTEM)
     writes land in a tm-owned workspace or the tm-global config dir. This inherits and depends on DOC-24
     §SPEC-STANDALONE-MPM-04; the #1927 fix restores that invariant at the `standalone/load.rs:176`
     drift site.
  3. **All writes inside a tm-owned tree.** Every PROJECT write targets `<workspace>/.claude/…` in a
     base clone, worktree, or managed clone tm provisioned.
- **Preconditions:** the target workspace is confirmed tm-owned (§02).
- **Postconditions:** after any F1/F2 run, a diff of the user's live checkout and the real `~/.claude`
  shows **zero** tm-authored changes; all new/updated content is confined to the tm-owned workspace.
- **Error conditions:** any operation that cannot confirm its write target is tm-owned, or that would
  resolve to the live checkout or the real `~/.claude*`, **fails closed** (no write) with a diagnostic —
  a degraded-but-segregated failure is acceptable; a working-but-polluting one is a contract violation.

#### Rationale (WHY)

This ID makes the 2026-07-02 segregation directive a mechanically testable invariant rather than a
convention. The confirmed cross-tool pollution incident happened precisely because a write escaped into a
shared/global tree; fail-closed (not fall-back-to-global) is mandatory because a silent escape is the exact
failure being eliminated. Pinning the invariant to its own ID lets it carry a dedicated isolation
integration test (Appendix A, WI-6) that asserts zero writes outside the tm-owned tree — the same style of
guard DOC-24 §04 uses.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::project_migrate` (new) | Read-only source access; tm-owned-target assertion; fail-closed guard. |
| `core::agent_deployer` / `core::skill_deployer` | Depend on the #1927-fixed target derivation; no `home_dir()` fallback in managed mode. |

---

### Observability — provisioning stages, shadow & migration logs {#SPEC-PROVISION-07~draft}

**ID:** SPEC-PROVISION-07~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Provisioning stages.** F1 migration and F2 pull emit provisioning-stage events through the existing
  `core::provisioning_stage::emit` machinery (the same seam `prepare_session_inner` uses for
  `DeployingAgents` / `DeployingSkills`, `mod.rs:276,303`), so a slow migration/pull is visibly "in
  flight" in the per-stage progress surface (#1904/#1919 lineage), not silent until completion.
- **Shadow records.** Every PROJECT-over-SYSTEM shadow (§01) is logged at `info` with the name, the
  winning PROJECT source, and the shadowed SYSTEM source, and is recorded in the per-project manifest so
  it is queryable after the fact ("why is bundled `reviewer` inactive?").
- **Migration records.** Each F1/F2 outcome per file — `migrated` / `unchanged` / `refreshed` /
  `preserved` (user-edited) / `flagged` (source-deleted) / `skipped` (unreadable) — is logged and
  counted, mirroring the deployers' existing `DeployResult`/`DeployStats` accounting
  (`agent_deployer.rs`, `skill_deployer.rs`).
- **Logs to stderr.** Per the daemon convention, all such logs go to **stderr** (never stdout, which
  stays clean for MCP framing).
- **Preconditions / Postconditions:** stage emission is a no-op outside a daemon `spawn_managed` scope
  (matching `provisioning_stage`), so unit tests and bare CLI runs are unaffected; a completed run leaves
  a queryable per-project record of shadows and migration outcomes.
- **Error conditions:** observability is best-effort — a logging/emission failure never aborts a
  migration or deploy.

#### Rationale (WHY)

The single most common question when agents change behavior is "which agent did I actually get, and why?"
Making shadows and migration outcomes **observable** (staged progress + structured logs + a queryable
per-project record) is what turns PROJECT-over-SYSTEM precedence from a black box into an auditable
decision, and reusing the existing `provisioning_stage` + `DeployResult` accounting means no new
observability substrate is invented.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::provisioning_stage` | Emit `MigratingProjectContent` / `PullingCatalogSubset` stages (new variants). |
| `core::project_migrate` (new) | Structured per-file outcome logs + counts; shadow records into the per-project manifest. |
| `core::agent_deployer` / `core::skill_deployer` | Emit shadow records when a SYSTEM file is shadowed by PROJECT. |

---

### Conformance matrix {#SPEC-PROVISION-08~draft}

**ID:** SPEC-PROVISION-08~draft
**Status:** Draft

Each row maps a requirement to the code path that will satisfy it and the observable check that will
confirm it. Status is `SPEC-ONLY` for every row: this is a design contract for **unbuilt** behavior — no
row claims `PASS` until its WI (Appendix A) lands with the cited test. (Rows follow the DOC-29 §2
column convention.)

| Row | Requirement | Implementing code path (target) | Observable verification (target) | Status |
|---|---|---|---|---|
| **PRV-01** | PROJECT > SYSTEM name layering; shadows recorded | `session_launch` applies PROJECT-over-SYSTEM by name; shadow record emitted | Deploy a workspace with a PROJECT `reviewer` + bundled `reviewer`; assert the effective agent is the PROJECT file and a shadow record names the bundled one | SPEC-ONLY |
| **PRV-02** | PROJECT home is `<workspace>/.claude/{agents,skills}`, tm-owned only | `managed_config` / `inproject` tm-owned assertion | Assert PROJECT writes land under the base clone / worktree; a live-checkout target fails closed | SPEC-ONLY |
| **PRV-03** | `ContentSource::Project` + `Origin::Project` producer; per-project ledger | `schema` enum variant; `agent_manifest`/`skill_manifest` producer; deployer skip on `Origin::Project` | Unit: round-trip `ContentSource::Project`; a SYSTEM deploy preserves an `Origin::Project` file | SPEC-ONLY |
| **PRV-04** | F1: non-platform detection + read-only extraction + placement | `project_migrate` enumerate/classify/copy | Fixture live checkout with 1 platform + 1 non-platform agent; assert only the non-platform one migrates, source unwritten | SPEC-ONLY |
| **PRV-05** | F1 idempotency: checksum-keyed re-run; user-edit preserve; source-delete flag | `project_migrate` idempotency via per-project ledger | Re-run migration ⇒ `unchanged`; edit workspace copy ⇒ `preserved`; delete source ⇒ `flagged`, file retained | SPEC-ONLY |
| **PRV-06** | F1 uncommitted content migrates | `inproject` live-checkout source vs base clone | Uncommitted non-platform file present in live checkout but not base clone ⇒ appears as PROJECT content | SPEC-ONLY |
| **PRV-07** | F2 requirement detection → selection manifest → catalog subset | `requirement_detect` + selection manifest + `catalog_sync` selection | Rust project fixture ⇒ selection includes `rust-engineer`, excludes unrelated agents; sources from existing catalog checkout | SPEC-ONLY |
| **PRV-08** | F2 pulls land as PROJECT content, ledgered, idempotent | `project_migrate` placement (shared with F1) | Assert pulled items carry `Origin::Project`; re-pull is idempotent; no second cache created | SPEC-ONLY |
| **PRV-09** | tm-agent-manager skill contract (detect → propose → confirm → pull → report) | tm-agent-manager skill drives `project_migrate` | Skill dry-run reports the proposed subset and, on confirm, the pulled set + shadows | SPEC-ONLY |
| **PRV-10** | Segregation: zero writes to live checkout or real `~/.claude*` | `project_migrate` fail-closed guard; #1927-fixed deploy target | Sandboxed `$HOME`: run F1/F2; assert zero writes to live checkout & real `~/.claude*`; all writes in tm-owned tree | SPEC-ONLY |
| **PRV-11** | Observability: stages + shadow/migration logs to stderr | `provisioning_stage` new variants; structured logs/counts | Assert `MigratingProjectContent`/`PullingCatalogSubset` stages emitted; per-file outcome counts logged to stderr | SPEC-ONLY |

---

## 3. Locked Design Decisions (Bob-approved defaults)

These four decisions are settled defaults with rationale; the spec is built on them.

| # | Decision | Rationale |
|---|----------|-----------|
| **D1** | **Project-level content home = `<workspace>/.claude/{agents,skills}` inside tm-OWNED workspaces** (base clone / worktree / managed clone). tm **NEVER** writes the user's live/primary checkout; F1 migration is **read-only** from the live checkout, writes go only to tm-owned trees. | Native Claude Code discovery (no flags, IDE-honored), matching DOC-24 §03/§06. Enforces the 2026-07-02 **segregation directive** issued after a confirmed cross-tool pollution incident via shared global `~/.claude/agents`. §SPEC-PROVISION-02 / -06. |
| **D2** | **Collision precedence = PROJECT shadows SYSTEM** (project > user > system), matching Claude Code's own layering. Shadows must be **logged/observable**. | Predictable to anyone who knows Claude Code; makes project content authoritative without deleting the SYSTEM fallback. Observable shadows keep "why did my agent change?" answerable. §SPEC-PROVISION-01 / -07. |
| **D3** | **F2 source = extend the existing `catalog_sync` machinery** (`content/catalog_sync.rs`, `~/.trusty-mpm/catalog`, repo `bobmatnyc/claude-mpm`, 24h TTL) with requirement-driven selection on top — do **NOT** invent a second cache concept. | One catalog, one TTL, one protected-skills rule (`/mpm-*`). F2 adds only a *selection* filter over the checkout that already exists (`catalog_sync.rs:39,43`). §SPEC-PROVISION-05. |
| **D4** | **The DOC-24 drift bug is out of scope** (filed as **#1927**, fixed first): `standalone/load.rs:176` uses `FrameworkPaths::default()` and deploys to the real `~/.claude`, violating SPEC-STANDALONE-MPM-04. This spec **DEPENDS** on that invariant holding. | Segregation (§06) cannot be guaranteed while a converged spawn path still writes the real `~/.claude`. Stating the dependency explicitly makes the ordering a gate, not an assumption. §Out-of-scope. |

---

## 4. Out of Scope & Dependencies

- **Dependency: #1927 (fix first).** The DOC-24 §SPEC-STANDALONE-MPM-04 drift — `run_prepare_session`
  building `FrameworkPaths::default()` (`standalone/load.rs:176`) and thereby deploying to the real
  `~/.claude` — must be fixed before this spec's segregation invariant (§06) can hold on the standalone
  path. This spec **consumes** the fixed invariant; it does not re-fix it.
- **Catalog repo split (future, not now).** Splitting the catalog source out of `bobmatnyc/claude-mpm`
  into a dedicated `claude-mpm-agents` repo is a **future option**, not part of this work; F2 targets the
  existing catalog repo/machinery (D3).
- **Not re-specified:** the DOC-24 command lexicon and `CLAUDE_CONFIG_DIR` isolation primitive; the
  session lifecycle / `SessionRecord`; the provisioner clone mechanics; the instruction pipeline and agent
  compose chain (DOC-29 BHV-01/04); the autonomy tiers (DOC-23).
- **Doc-drift work items (noted, tracked in the WI plan):** the mpm-agent-manager agent doc and the
  tm-agent-architecture skill do **not** currently mention the catalog source at all; F2 requires them to
  document it (Appendix A, WI-5).

---

## Appendix A — Implementation Plan (WI breakdown)

> Scopes: **S** ≈ ≤1 day, **M** ≈ 2–4 days, **L** ≈ ≥1 week. To be filed as GitHub issues after spec
> acceptance; each WI is one shippable increment.

| WI | Scope | Work | Realizes | Depends on |
|----|-------|------|----------|------------|
| **WI-1** | **S** | **Ledger + schema extensions.** Add `ContentSource::Project` (`schema.rs`, default unchanged); add `Origin::Project` producer + give `Origin::User` a producer (`agent_manifest`/`skill_manifest`); make the deployer classify step tier-aware (never overwrite `Origin::Project`; stop hardcoding `Origin::Bundled` where a producer applies). | SPEC-PROVISION-03, -01 | #1927 |
| **WI-2** | **M** | **`core::project_migrate` module + segregation guard.** Enumerate/classify (platform vs non-platform), read-only copy into `<workspace>/.claude/…`, ledger `Origin::Project`, idempotency by source-checksum, tm-owned-target assertion + fail-closed guard. Shared by F1 and F2 placement. | SPEC-PROVISION-04, -06, -02 | WI-1 |
| **WI-3** | **M** | **F1 wiring into the in-project spawn.** Resolve live-checkout source vs tm-owned base clone/worktree in `inproject`; invoke `project_migrate`; handle uncommitted content, shadows, source-deleted flagging; propagate PROJECT content to managed-clone sessions. | SPEC-PROVISION-04, -01 | WI-2 |
| **WI-4** | **M** | **F2 requirement detection + selection + catalog extension.** New `core::requirement_detect` (marker-file → tags); data-driven selection manifest (tag → catalog name); requirement-driven selection over the existing `catalog_sync` checkout (no new cache); land via `project_migrate`. | SPEC-PROVISION-05, -03 | WI-2 |
| **WI-5** | **S** | **tm-agent-manager skill contract + doc drift.** Wire the detect → propose → confirm → pull → report flow into the tm-agent-manager skill; update the mpm-agent-manager agent doc and tm-agent-architecture skill to document the catalog source (currently unmentioned). | SPEC-PROVISION-05 | WI-4 |
| **WI-6** | **M** | **Observability + isolation integration tests.** Add `MigratingProjectContent` / `PullingCatalogSubset` provisioning-stage variants; structured per-file outcome logs/counts + shadow records; sandboxed-`$HOME` integration test asserting zero writes to the live checkout or real `~/.claude*` (the §06 invariant) and the PRV-01…PRV-11 checks. | SPEC-PROVISION-07, -06, -08 | WI-2, WI-3, WI-4 |

**Critical path:** #1927 → WI-1 → WI-2 → (WI-3 ∥ WI-4) → WI-5, WI-6. WI-3 and WI-4 parallelize after
WI-2; WI-5 follows WI-4; WI-6 (tests/observability) closes out after the feature WIs.

---

## 5. Assumptions & Risks

| # | Assumption / Risk | Status | Mitigation / WI |
|---|-------------------|--------|-----------------|
| A1 | The #1927 fix lands first, restoring the DOC-24 §04 real-`~/.claude` exclusion on the standalone path. | **Dependency (gating).** | Sequence #1927 before WI-2; §06 tests assert the invariant regardless. |
| A2 | "not-in-manifest ⇒ non-platform" correctly separates project content from framework content. | **Verified** (mirrors `agent_deployer.rs:145`). | WI-1/WI-2 reuse the exact classification; tests cover platform/non-platform split. |
| A3 | The base clone's **committed** `.claude/` may diverge from the live checkout's **uncommitted** content. | **Expected** (§1.4). | F1 treats the live checkout as authoritative source; logs divergence (WI-3). |
| A4 | Requirement detection is inherently heuristic; unknown stacks yield no tags. | **Accepted.** | Best-effort/additive; empty tags ⇒ no-op, never an error (WI-4). |
| A5 | Concurrent F1/F2 on the same workspace could race on `.claude/` + ledger. | **Risk.** | Reuse the deployers' atomic-write + per-directory manifest; per-workspace advisory lock (WI-2, document at impl). |
| A6 | Catalog checkout may be stale/absent at F2 time. | **Handled.** | Use on-disk content or trigger the existing sync; never a parallel fetch (D3, WI-4). |

---

## 6. Open Questions / Future Work

1. **Selection-manifest home.** Should the F2 tag→content selection manifest ship bundled, live in the
   catalog repo, or be project-overridable via the HR-2 manifest? Decide in WI-4.
2. **Shadow-resolution UX.** Beyond logging, should `tm doctor` surface active shadows for a project?
   Candidate follow-up after WI-6.
3. **Catalog repo split.** If/when the catalog moves to a dedicated `claude-mpm-agents` repo, F2's source
   pointer changes but its selection logic does not (D3 keeps them decoupled).
4. **PROJECT content lifecycle.** Should a source-deleted PROJECT file ever be auto-removed from the
   workspace (vs today's flag-and-retain, §04)? Deferred; retain-by-default is the safe start.

---

## 7. References

- [DOC-24 — Standalone Managed `trusty-mpm` Driver](./standalone-managed-trusty-mpm.md) — project-local `repo/.claude/` model; §SPEC-STANDALONE-MPM-04 isolation invariant (the dependency).
- [DOC-17 — Autonomous Multi-Session Managed Harness Runner](./harness-runner-vision.md) — HR-2 manifest precedence; catalog sync.
- [DOC-29 — Primary trusty-mpm Harness Behaviors](./mpm-behavior-conformance.md) — BHV-04 (agent/skill bundling), BHV-06 (catalog sync); conformance-row house style.
- `crates/trusty-mpm/src/core/session_launch/mod.rs` — `prepare_session_inner` (238); `deploy_agents_filtered` → `claude_agents_dir()` (279); `deploy_skills_filtered` → `claude_skills_dir()` (307).
- `crates/trusty-mpm/src/core/agent_deployer.rs` — `deploy_agents_filtered` (92–192); classify (143–162); `Origin::Bundled` hardcode (180).
- `crates/trusty-mpm/src/core/agent_manifest.rs` — `MANIFEST_FILE` (22); `Origin` enum (87–94); `atomic_write` (51).
- `crates/trusty-mpm/src/core/skill_deployer.rs` / `skill_manifest.rs` — the skill-side parallel.
- `crates/trusty-mpm/src/core/manifest/schema.rs` — `ContentSource` (126–134). `resolve.rs` — `resolve_manifest` precedence (95–116).
- `crates/trusty-mpm/src/content/catalog_sync.rs` — `catalog_root_for` (39); `DEFAULT_CATALOG_REPO = bobmatnyc/claude-mpm` (43).
- `crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs` — `spawn_managed_cloned` (224). `inproject.rs` — base clone + `.worktrees/`; `DEFAULT_REPOS_DIR` (41).
- `crates/trusty-mpm/src/core/standalone/load.rs` — `run_prepare_session` (176) — the #1927 drift site.
- `crates/trusty-mpm/src/bin/tm/commands/project.rs` — `scaffold_project_dir` (105).
- `crates/trusty-mpm/src/core/paths.rs` — `agent_source_dir` / `skill_source_dir` (231–258).

---

## 8. Change log

- **2026-07-02** — Initial draft (DOC-31, `SPEC-PROVISION-01~draft` … `-08~draft`). Defines the
  SYSTEM/PROJECT/USER-GLOBAL taxonomy and PROJECT > SYSTEM precedence; ledger/schema extensions
  (`ContentSource::Project`, `Origin::Project`); F1 in-project migration of non-platform content
  (read-only from the live checkout into tm-owned workspaces); F2 requirement-driven scaffolding pulls
  over the existing `catalog_sync` checkout; the segregation invariants; observability; and an 11-row
  conformance matrix (all `SPEC-ONLY`). Records the four locked design decisions (D1–D4) and the #1927
  dependency. Implementation decomposed into six WIs (Appendix A).
