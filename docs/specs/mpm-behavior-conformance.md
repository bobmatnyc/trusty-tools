# DOC-29 — Primary trusty-mpm Harness Behaviors — Conformance Matrix

**Status:** Draft
**Subsystem:** trusty-mpm — behavior conformance / cross-spec verification
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-07-01
**Spec ID:** `SPEC-MPM-BEHAVIOR-01~draft` … `SPEC-MPM-BEHAVIOR-06~draft` (DOC-29)
**Builds on:** DOC-17 — Autonomous Multi-Session Managed Harness Runner
(`docs/specs/harness-runner-vision.md`); DOC-28 — trusty-mpm Self-Awareness and
Instruction-Load Verification (`docs/specs/trusty-mpm-self-awareness.md`);
[Three-Harness Architecture](../architecture/harnesses.md); Session Manager
Daemon MVP spec (`docs/trusty-mpm/spec/SESSION_MANAGER_MVP.md`)
**Cross-ref:** `crates/trusty-mpm/src/core/session_launch/`,
`crates/trusty-mpm/src/core/instruction_pipeline.rs`,
`crates/trusty-mpm/src/core/agent_builder.rs`,
`crates/trusty-mpm/src/core/agent_deployer.rs`,
`crates/trusty-mpm/src/core/skill_deployer.rs`,
`crates/trusty-mpm/src/core/manifest.rs`,
`crates/trusty-mpm/src/core/update_check/`,
`crates/trusty-mpm/src/content/catalog_sync.rs`,
`crates/trusty-mpm/src/daemon/doctor_output_style.rs`,
`crates/trusty-mpm/src/assets/`

> **Scope note.** This spec is a **consolidation, not a new design**. The
> behaviors it enumerates are already specified across DOC-17, DOC-28, the
> Three-Harness Architecture doc, and the Session Manager MVP spec — this
> document adds no new behavior contracts. Its job is to give trusty-mpm's
> **primary, user-distinguishing harness behaviors** (the things that make
> trusty-mpm a unique meta-harness, as opposed to a bare wrapper around
> `claude`) a single, testable, per-behavior conformance table with a real
> source-code citation and an observable verification command for each row.
> It **excludes observability/monitoring internals** — the LLM activity
> monitor, tmux pane-reading/classification, and the pending-decision API
> (`docs/trusty-mpm/spec/SESSION_MANAGER_MVP.md` §10) are deliberately out of
> scope here; they are monitors, not primary behaviors, per the owner's
> framing. Where a canonical source document already states the full prose for
> a behavior, this document **links** to it rather than duplicating it.

---

## 1. How to read this document

Each row in §2 is one **primary behavior** (`BHV-NN`). The columns are:

| Column | Meaning |
|---|---|
| **Behavior ID** | Stable ID, `BHV-NN`, referenced from tickets/PRs/tests. |
| **What it does** | One-line summary — not a restatement of the canonical spec, just enough to orient a reader. |
| **Canonical source** | The spec section (with `§` anchor where one exists) that is the actual behavior contract. This document does not re-derive that contract. |
| **Implementing code** | Real `file:line`/function citations in the current tree (verified 2026-07-01 against `origin/main` @ `a7e6f546` / trusty-mpm 0.14.0). |
| **Observable verification** | The exact command run and the exact artifact/assertion that constitutes a pass. Every command in this column was actually executed during authoring of this spec (see §3 for the harness). |
| **Status** | `CONFIRMED PASS` (executed and observed passing this pass), `PARTIALLY-IMPLEMENTED` (some sub-behavior confirmed, some not), or `NEEDS-VERIFICATION` (spec'd, code exists, not exercised this pass). No row claims `CONFIRMED PASS` without a command+output backing it — see §3. |

---

## 2. Conformance matrix

### BHV-01 — Canonical instruction assembly + agent authority

**What it does:** Assembles the PM's non-overridable system prompt floor
(prohibitions, delegation authority, mandatory 5-phase workflow) at session
prep time, in a fixed precedence order that project-level overrides cannot
silence.

**Canonical source:** [DOC-17 §2.1](./harness-runner-vision.md#21-instruction-assembly--implemented-pr-1389)
("Instruction assembly — implemented"); [Three-Harness Architecture — trusty-mpm
§](../architecture/harnesses.md#2-trusty-mpm--the-meta-harness).

**Implementing code:**
- `crates/trusty-mpm/src/core/instruction_pipeline.rs` — concatenation order
  `PM_INSTRUCTIONS → WORKFLOW → AGENT_DELEGATION → BASE_PM` (lines 67–72);
  `install_system_prompt()` / `install_system_prompt_to()` (lines 84, 100).
- `crates/trusty-mpm/src/assets/instructions/PM_INSTRUCTIONS.md` — `## Prohibitions
  (CANONICAL — single source of truth)` (line 11), `## Workflow (5-phase)`
  (line 143).
- `crates/trusty-mpm/src/assets/instructions/WORKFLOW.md` — `## Mandatory
  5-Phase Sequence` (line 5): Research (conditional) → Code Analysis Review
  (mandatory) → Implementation → QA (mandatory, blocking gate) → Documentation.
- `crates/trusty-mpm/src/assets/instructions/AGENT_DELEGATION.md`,
  `crates/trusty-mpm/src/assets/instructions/BASE_PM.md` (non-overridable
  floor: `> Always appended to PM prompt. Cannot be overridden.`).
- `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md` — SM-side floor
  mirror (`## Non-Overridable Rules`, `## Trusty Tool Priority
  (Non-Overridable)`).

**Observable verification:**
```
cargo test -p trusty-mpm --lib -- instruction_pipeline
```
8/8 pass, including `assemble_system_prompt_contains_all_sections` and the
floor-ordering assertion (`base > delegation`, i.e. `BASE_PM` is proven to be
appended last / non-overridable). Direct file read confirms the literal
headings cited above.

**Status:** CONFIRMED PASS (0.14.0, direct execution 2026-07-01).

---

### BHV-02 — Self-awareness

**What it does:** Gives a launched session a bundled canonical "what am I"
doc, a non-overridable Identity & Self-Awareness Protocol that routes identity
questions to memory + that doc (never shell-probing), and a deterministic
external `tm doctor` check that catches a session's `outputStyle` silently
failing to resolve.

**Canonical source:** [DOC-28](./trusty-mpm-self-awareness.md) R1–R4 in full.

**Implementing code:**
- `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` — the R1 canonical doc; opens
  "trusty-mpm is a **Rust** crate at `crates/trusty-mpm/`… **`tm`**…".
- `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md:40-41` — load
  marker `<!-- trusty-mpm-instructions-loaded: v1 -->` immediately followed by
  `## Identity & Self-Awareness Protocol (Non-Overridable)`.
- `crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md:78-79`,
  `trusty-mpm-research.md:100-101`, `trusty-mpm-teacher.md:98-99` — same
  marker + heading pair mirrored into all three bundled styles.
- `crates/trusty-mpm/src/daemon/doctor_output_style.rs` — `check_output_style()`
  (line 42): `Ok` when the effective `outputStyle` id resolves to a real,
  non-empty on-disk file; `Warn` when the key is absent; `Fail` when the id is
  unknown (the literal incident condition) or the file is missing/empty or the
  settings JSON is malformed.

**Observable verification:**
```
grep -n "trusty-mpm-instructions-loaded" crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md \
  crates/trusty-mpm/src/assets/output-styles/*.md
cargo test -p trusty-mpm --lib -- doctor_output_style
cargo test -p trusty-mpm --lib -- run_doctor_produces_seven_checks
```
The grep confirms the marker is present in all 4 assets, immediately preceding
the protocol heading. `doctor_output_style` — 7/7 pass, including
`output_style_fail_when_id_unknown` (asserts `CheckStatus::Fail` and a message
containing `"claude_mpm"` — this **is** `check_output_style` returning `Fail`
on `outputStyle: "claude_mpm"`, the exact reproduction of the DOC-28 incident)
and `output_style_ok_when_style_resolves`. `run_doctor_produces_seven_checks`
confirms the probe is wired into `run_doctor`'s report (up from 6 checks
pre-DOC-28).

**Status:** CONFIRMED PASS (0.14.0, direct execution 2026-07-01).

---

### BHV-03 — Memory + search integration

**What it does:** Every session `prepare_session` launches gets `trusty-memory`
and `trusty-search` wired in as stdio MCP servers in that session's
`.mcp.json`, plus a `trusty-memory` hook block and (issue #1373/#1605) a
palace/index pinned to the project.

**Canonical source:** [SESSION_MANAGER_MVP.md §"Canonical-Context Preservation
Principle"](../trusty-mpm/spec/SESSION_MANAGER_MVP.md#1-canonical-context-preservation-principle)
— the bundle enumeration: agents, skills, system prompt, `.mcp.json`,
`.claude/settings.json`, memory palace links, hooks.

**Implementing code:**
- `crates/trusty-mpm/src/core/session_launch/mod.rs` — `prepare_session_inner`
  (lines ~344–407): writes the `trusty-memory` hook block, injects the
  `trusty-memory` MCP server, registers + pins the project's `trusty-search`
  index, injects the `trusty-search` MCP server, then removes the now-redundant
  **global** `trusty-memory` hook entries.
- `crates/trusty-mpm/src/core/session_launch/settings.rs` —
  `inject_trusty_memory_mcp_server` (line 376), `inject_trusty_search_mcp_server`
  (line 474), the shared `inject_mcp_server` primitive (line 317).

**Observable verification:**
```
cargo test -p trusty-mpm --lib -- session_launch
```
180/180 pass, including `prepare_session_injects_both_mcp_servers`,
`inject_trusty_memory_mcp_uses_serve_stdio`, `inject_trusty_search_mcp_pins_index`,
and `inject_both_mcp_servers_coexist` — each directly parses the generated
`.mcp.json` and asserts it contains both `trusty-memory` and `trusty-search` as
stdio servers (this is the exact observation: **"generated `.mcp.json`
contains `trusty-memory` + `trusty-search` stdio servers"**).

**Status:** CONFIRMED PASS (0.14.0, direct execution 2026-07-01).

---

### BHV-04 — Agent + skill bundling/loading

**What it does:** Deploys the compiled-in catalog of agent and skill
definitions to `~/.claude/agents/` / `~/.claude/skills/` at session prep;
agents compose via a base-first `extends:` frontmatter chain
(`base-agent → base-engineer → <leaf>`) into one merged file, with
`initialPrompt` and resource-tier→model defaults injected at deploy time.

**Canonical source:** [DOC-17 §2.2](./harness-runner-vision.md#22-agent-base-hierarchy--implemented-and-correct)
("Agent BASE hierarchy — implemented and correct") and **HR-1**
(§3, BASE content parity + `initialPrompt` + tier→model defaults).

**Implementing code:**
- `crates/trusty-mpm/src/core/agent_builder.rs` — `compose_agent()` (line 503),
  `source_chain()` (line 528), with cycle detection + depth cap.
- `crates/trusty-mpm/src/core/agent_deployer.rs` — `deploy_agents()` (line 70);
  `initialPrompt` + tier→model default injection at deploy time.
- `crates/trusty-mpm/src/core/skill_deployer.rs` — `deploy_skills()` (line 74).
- `crates/trusty-mpm/src/assets/agents/` — **41 files**: 5 base templates
  (`BASE-AGENT.md`, `BASE-ENGINEER.md`, `BASE-QA.md`, `BASE-OPS.md`,
  `BASE-RESEARCH.md`) + 36 concrete agents, incl. `rust-engineer.md`
  (frontmatter `extends: base-engineer`) → `BASE-ENGINEER.md` (frontmatter
  `extends: base-agent`) → `BASE-AGENT.md`.
- `crates/trusty-mpm/src/assets/skills/` — **12 real guidance skills** (the
  11 consts in `bundle_skills.rs` + `tm-doctor.md`) plus one placeholder,
  `example-skill.md`, that is not part of the real catalog.

**Observable verification:**
```
cargo test -p trusty-mpm --lib -- agent_builder agent_deployer skill_deployer bundle
```
All pass, including `new_concrete_agents_deploy_via_real_asset_files` — which
composes every real bundled agent file (including `rust-engineer`) against the
**real on-disk assets** (not fixtures), catching `extends:` typos or missing
base templates — and `source_chain_engineer`, which asserts the resolved chain
order `["base-agent", "base-engineer", "engineer"]`. Also
`deploy_injects_initial_prompt_and_tier_model` (HR-1's deploy-time
enrichments). Direct read confirms `rust-engineer.md`'s frontmatter composes
`base-engineer → base-agent` (this **is** the "41 agents / 12 skills bundled,
rust-engineer composes base-agent→base-engineer" observation).

**Status:** CONFIRMED PASS (0.14.0, direct execution 2026-07-01).

---

### BHV-05 — Autonomous provisioning + lifecycle

**What it does:** Provisions an mpm-owned, isolated workspace from
`(repo_url, ref, task)`, runs `prepare_session` inside it, spawns/observes/
resumes/decommissions the managed tmux session, and reconciles session state
on daemon restart — the G0 "operator manages nothing" behavior.

**Canonical source:** [DOC-17 §1](./harness-runner-vision.md#1-north-star-the-guiding-principle)
(G0 north-star); [SESSION_MANAGER_MVP.md §6](../trusty-mpm/spec/SESSION_MANAGER_MVP.md#6-workspace-provisioner)
(Workspace Provisioner) and [§9](../trusty-mpm/spec/SESSION_MANAGER_MVP.md#9-session-lifecycle--naming)
(Session Lifecycle & Naming).

**Implementing code:**
- `crates/trusty-mpm/src/provisioner/workspace.rs` — `WorkspaceProvisioner`,
  isolation under `~/.trusty-mpm/workspaces/<project>/<session-id>/`.
- `crates/trusty-mpm/src/session_manager/` — `SessionManager` (naming
  convention, spawn/send/stop/resume/decommission/prune, `reconcile_on_boot`).
- `crates/trusty-mpm/src/core/manifest.rs` (`core::manifest::resolve`) —
  the HR-2 **manifest-driven provisioning precedence**
  (project override > user config > catalog manifest > compiled-in default),
  consumed by `prepare_session_inner` via `HarnessPlan::from_manifest`.

**Observable verification:**
```
cargo test -p trusty-mpm --lib -- provisioner:: session_manager::
cargo test -p trusty-mpm --lib -- manifest
```
106/106 pass for `provisioner`/`session_manager` (fake tmux/git backends per
the MVP spec's test strategy) — including `provisioner_isolation_path`,
`provisioner_path_not_in_existing_project`, `manager_naming_convention`,
`manager_reconcile_gone_tmux_yields_stopped`, and the prune/decommission suite.
61/61 pass for `manifest`, including `resolve_project_wins`,
`resolve_user_over_catalog`, and `resolve_catalog_over_default` — these three
tests are the direct confirmation that HR-2's precedence order is implemented,
**which is further along than DOC-17's own 2026-06-17 audit states** (DOC-17
§3 HR-2 was written as pending near-term work; as of this pass it is wired
into `prepare_session_inner` and unit-verified).

**What was NOT run this pass:** a live end-to-end smoke test — a real
`tm sessions new --repo <url> --ref main --task "…"` against a real tmux
binary and a real git remote (`SESSION_MANAGER_MVP.md §13`, acceptance
criteria 2/3/5/6/11/12; the crate's own `#[ignore]`-tagged
`test_live_session_e2e`). Everything above is unit/fake-backend level.

**Status:** PARTIALLY-IMPLEMENTED — unit-verified with fake backends (workspace
isolation, naming, reconciliation, manifest precedence all pass); the
live-tmux/live-git E2E path is spec'd (with an explicit `#[ignore]` smoke test
already in the tree) but was not executed in this pass. Do not read
"unit-verified" as "live-verified."

---

### BHV-06 — Content freshness / catalog sync

**What it does:** Fetches and caches the agent/skill catalog from the
upstream claude-mpm repo with a configurable TTL (`CatalogSync`); detects
staleness against the deployed checksum manifest and surfaces it on `/health`
and the coordinator TUI; offers (does not force) a redeploy.

**Canonical source:** [DOC-17 §2.3](./harness-runner-vision.md#23-catalog-sync--implemented-not-wired)
("Catalog sync — implemented, NOT wired") and **HR-3** (§3, update-check +
rebuild offer); [SESSION_MANAGER_MVP.md §7](../trusty-mpm/spec/SESSION_MANAGER_MVP.md#7-content-sync-from-claude-mpm-repository).

**Implementing code:**
- `crates/trusty-mpm/src/content/catalog_sync.rs` — `CatalogSync` fetch/cache/
  TTL, `list_agents()`/`list_skills()`; `.sync()`'s only production call site
  is `crates/trusty-mpm/src/bin/tm/commands/managed.rs:549` (the `tm catalog
  sync` CLI command).
- `crates/trusty-mpm/src/core/update_check/mod.rs` + `apply/` — staleness
  `detect_for_framework()` (checksum/hash compare vs. the deployed manifest)
  and `apply()` (the redeploy/prune "offer").
- `crates/trusty-mpm/src/daemon/api.rs:305-323` — `/health` returns
  `catalog_stale` / `catalog_unknown`, computed live on every health check.
- `crates/trusty-mpm/src/tui/coordinator/{poll,layout,render}.rs` — the DOC-16
  TUI staleness indicator, driven by the same `/health` field.

**Observable verification:**
```
cargo test -p trusty-mpm --lib -- catalog_sync
cargo test -p trusty-mpm --lib -- update_check
```
11/11 `catalog_sync` tests pass (TTL bypass/skip, URL normalization,
`catalog_ls_lists_agents`/`catalog_ls_lists_skills`, path-escape guarding).
15/15 `update_check` tests pass, including `apply_redeploys_and_clears_staleness`
(the "rebuild offer, accepted" acceptance flow) and `detect_flags_new_agent`/
`detect_flags_changed_skill` (the checksum-compare detection). This is **also
further along than DOC-17's 2026-06-17 audit**, which described catalog sync
as fetch-only and "NOT wired" for staleness detection — `/health`'s
`catalog_stale` field and the TUI indicator are both present and unit-tested
as of this pass.

**What is still genuinely a gap:** `CatalogSync::sync()` (the actual upstream
*fetch*) has exactly one production call site — the manual `tm catalog sync`
CLI command. There is no automatic fetch-on-launch or fetch-on-daemon-start;
`prepare_session_inner` reads whatever is **already** on disk under
`catalog_root_for()`. Staleness *detection* is autonomous (every `/health`
poll); staleness *remediation* (fetching the new content) is not.

**Status:** PARTIALLY-IMPLEMENTED — fetch/cache/TTL, staleness detection, and
the `/health`+TUI surfacing are all implemented and unit-tested (ahead of
DOC-17's written audit); the actual upstream fetch remains a manual step, not
an autonomous one. Live-daemon `/health` polling against a real running
`trusty-mpmd` was not exercised this pass (API-level unit tests only).

---

## 3. How to run the conformance check

The commands in §2's **Observable verification** rows are all `cargo test`
invocations against the existing unit-test suites — no separate harness is
required to reproduce them; they are the same commands anyone runs locally.

For the safe, hermetic method to exercise the *production functions*
(`prepare_session`, `install_to`, `deploy_agents`, `deploy_skills`,
`check_output_style`) directly, without touching a real `~/.claude` or
`~/.trusty-mpm`, use `FrameworkPaths::under(tempdir)` plus a scratch project
directory:

```rust
let scratch_home = tempfile::tempdir()?;
let fw = FrameworkPaths::under(scratch_home.path());       // never touches real ~/.trusty-mpm
let scratch_project = tempfile::tempdir()?;
let report = prepare_session(&fw, scratch_project.path())?; // exercises BHV-01/03/04
check_output_style(Some(scratch_project.path()), scratch_home.path()); // exercises BHV-02
```

This is exactly the pattern the crate's own test modules
(`core::session_launch::tests`, `daemon::doctor::doctor_output_style::tests`,
`core::agent_deployer::tests`) already use — `FrameworkPaths::under` plus
`tempfile::TempDir` fixtures — which is why running the existing `cargo test`
suites in §2 constitutes a real, isolated exercise of the production code
paths rather than a mock of them. No test in this pass wrote to a real
`~/.claude` or `~/.trusty-mpm` directory.

To reproduce the full sweep used while authoring this document:

```bash
cargo test -p trusty-mpm --lib -- session_launch doctor bundle agent_builder output_style
cargo test -p trusty-mpm --lib -- agent_deployer skill_deployer install catalog_sync
cargo test -p trusty-mpm --lib -- provisioner:: session_manager:: manifest update_check instruction_pipeline
```

---

## 4. Known gaps

- **Output-style isolation leak (bug #1860).** `check_output_style` (BHV-02)
  and `deploy_output_style`/related tests write into a **global**-shaped
  `<home>/.claude/` tree; a test or a manual invocation pointed at a real
  `$HOME` risks bleeding into the operator's actual global settings if the
  `home` argument is not scrupulously scoped to a tempdir. §3's
  `FrameworkPaths::under(tempdir)` pattern is the mitigation; #1860 tracks
  hardening this so the isolation is structural rather than
  convention-dependent.
- **Flaky test (bug #1858).** At least one test in the suites exercised in
  §2/§3 has a known intermittent-failure history tracked as #1858. It did not
  fail during this pass's execution, but a red run of any command in §2/§3
  should be cross-checked against #1858 before being treated as a genuine
  regression.
- **R3 auto-seed deferred (DOC-28 §7 Phase 5, §10).** The trusty-memory
  prompt-fact identity seed (DOC-28 R3) is a documented **manual** `kg_assert`
  step today; automatic idempotent seeding from `prepare_session` is explicitly
  deferred future work, not required for BHV-02's Draft acceptance.
- **BHV-05/06 live-path gap (this spec).** As detailed in both rows above:
  workspace provisioning, session lifecycle, catalog sync, and staleness
  detection are all unit-verified with fake backends; none of the
  live-tmux / live-git-clone / live-daemon-`/health` paths were exercised in
  this pass. Treat BHV-05/06's `PARTIALLY-IMPLEMENTED` status as "implemented
  and unit-tested, live path not reproduced here," not as "half-built."
- **DOC-17's audit table is stale.** DOC-17 §2 was written 2026-06-17 and
  describes HR-1/HR-2/HR-3 as pending near-term work. As of this pass (0.14.0,
  2026-07-01) all three appear implemented and unit-tested (see BHV-04's
  `initialPrompt`/tier-model injection, BHV-05's manifest precedence, and
  BHV-06's staleness detection). This document does not edit DOC-17 directly
  (out of scope for a conformance matrix), but a follow-up PR should refresh
  DOC-17 §2/§4's status table to avoid the two documents drifting further
  apart.

---

## 5. Change log

- **2026-07-01** — Initial draft (DOC-29, `SPEC-MPM-BEHAVIOR-01~draft` …
  `-06~draft`). Consolidates the primary, user-distinguishing trusty-mpm
  harness behaviors (instruction assembly + agent authority, self-awareness,
  memory/search integration, agent/skill bundling, autonomous provisioning,
  content freshness) scattered across DOC-17, DOC-28, the Three-Harness
  Architecture doc, and the Session Manager MVP spec into one conformance
  matrix. BHV-01 through BHV-04 are `CONFIRMED PASS` via direct execution
  against 0.14.0. BHV-05/06 are `PARTIALLY-IMPLEMENTED`: unit/fake-backend
  verified, live-path not reproduced this pass. Notes that DOC-17's own audit
  table under-counts current progress on HR-1/HR-2/HR-3.

---

## References

- [DOC-17 — Autonomous Multi-Session Managed Harness Runner](./harness-runner-vision.md)
- [DOC-28 — trusty-mpm Self-Awareness and Instruction-Load Verification](./trusty-mpm-self-awareness.md)
- [Three-Harness Architecture](../architecture/harnesses.md)
- [Session Manager Daemon: MVP Spec](../trusty-mpm/spec/SESSION_MANAGER_MVP.md)
- [Spec catalog](./README.md)
