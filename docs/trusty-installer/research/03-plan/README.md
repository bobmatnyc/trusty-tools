# 03-plan — trusty-controller / `tctl` Completion Plan

This directory holds the **phased implementation plan** for completing the
`trusty-controller` crate (binary `tctl`) against its **02-design** doc set, the
**hard adversarial review** that shaped it, plus the **living progress manifest**
that tracks execution.

## Strategic choice — Option A vs Option B (decide this first)

The adversarial review ([`02-adversarial-review.md`](./02-adversarial-review.md))
established that the full contract/dispatch rebuild is **not** an unconditional
spine. The campaign faces an explicit choice, decided at a **DECISION GATE after
P0/P2**:

- **Option A — value-first minimal sub-campaign.** Ship **P0** (manifest + README
  rewrite + collapse the 4 divergent member lists + real `stack_version`) **and
  the P2 lattice** as a **standalone deliverable**. This alone fixes the lying
  README, the four-list correctness hazard, and the load-bearing *"unindexed
  project ≠ broken stack"* rollup bug — with **no identity flip** and **no
  cross-crate epic**. Low-risk, self-contained, independently valuable.
- **Option B — full contract/dispatch rebuild** (P1 / P3 / P4a–P4d / P5, + P6
  migration + P7a/P7b harness). Per the plan's own caveat (a), the P1 contract
  module is **worthless unless the P4 cross-crate retrofit is funded in the same
  campaign** — so **P1+P4 must be justified and committed together**, not assumed
  as the spine.

Do not enter P1 until the DECISION GATE records "proceed to Option B" **with** the
P1+P4 joint-funding commitment (decision **D-OptAB**).

## Current state (one paragraph)

`crates/trusty-controller/` is at `v0.2.0` and is **further along than its README
admits** (the README still claims "Phase 0 scaffold" / `publish = false`, both
false). A large surface is shipped and working — `install`, `upgrade`/`updates`,
`ensure --wait`, launchd `start`/`stop`/`restart`, `config`, `port`, `doctor
--self-check`, `ui` (launches `trusty-console` per ADR-0011), and the full `up/`
bootstrap orchestrator (STAGE 0–5, real `flock` lock). But it is built on
**stubbed foundations**: the rollup is a flat 2-value fold (not the DOC-4 lattice),
envelope validation is ad-hoc `serde_json::Value` (not a typed `Envelope<T>`),
`version` emits `0.0.0-scaffold`, scope is unwired (`let _ = cli.scope`,
TODO #920), `src/commands/passthrough.rs` is a stub, and **four divergent hard-coded member
lists** stand in for the missing manifest. **Four foundations do not exist yet**:
the `trusty_common::contract` module (DOC-1), the manifest/BOM loader (DOC-2),
scope threading (DOC-3), and the real rollup (DOC-4). The 214 tests are all
pure-function unit tests and will not catch dispatch/scope regressions. Full
detail: [`00-current-state.md`](./00-current-state.md).

## Files in this directory

| File | Purpose |
|---|---|
| [`00-current-state.md`](./00-current-state.md) | Self-contained snapshot: DONE / SUBSET-STAND-IN / DEAD / STUB / MISSING, with the four missing foundations and four divergent member lists. Read first. |
| [`01-phased-plan.md`](./01-phased-plan.md) | **The plan.** Phases **P0, P2, [DECISION GATE], P1, P4a, P3, P4b/c/d, P5, P6, P7a, P7b, P8** with enter gates, PR-sized work items, binary exit gates, risks, blast radius, CROSS-PHASE INVARIANTS, a backlog section, and an ASCII phase-ordering diagram. |
| [`02-adversarial-review.md`](./02-adversarial-review.md) | **The hard review.** All findings (BLOCKERS B1–B3, MAJORS M1–M5, MINORS m1–m6, the whole-approach critique, TOP-5), each with a "Resolution in plan" line and the OPEN DECISIONS it raised. Read alongside the plan to understand *why* the phase graph is shaped this way. |
| [`progress-manifest.md`](./progress-manifest.md) | **Living tracker.** Phase-status table, per-phase work-item checklists, backlog tracker, decisions log (incl. OPEN DECISIONS), deviations log, caveat watch. **Must be updated at every phase gate** (advisory/honor-system — see manifest m5). |

## How to use this plan

1. **Read [`00-current-state.md`](./00-current-state.md)** to understand what is
   built vs. designed, then skim
   [`02-adversarial-review.md`](./02-adversarial-review.md) to see why the phase
   graph is shaped the way it is.
2. **Work the phases in dependency order** from
   [`01-phased-plan.md`](./01-phased-plan.md): **P0 → P2 → DECISION GATE → (Option
   B) P1 → P4a → P3 → P4b/c/d → P5 → P6 → P7a → P7b → P8.** Do not start a phase
   until its **ENTER GATE** preconditions are met. In particular: do not enter P1
   until D-OptAB chooses Option B with P1+P4 joint funding; P4a gates P3's exit;
   the slug-flag flip happens only in P6 (atomic with the migration).
3. **At every gate, update [`progress-manifest.md`](./progress-manifest.md)** —
   record the date, PR #, and status, and tick the work-item checkboxes. The
   manifest is the authoritative state of the campaign; keeping it current is part
   of the definition of done for each phase.
4. **Treat exit gates as binary pass/fail.** Each is phrased so it is mechanically
   verifiable (a test is green, a grep returns nothing, a `--json` field has a
   specific value). A phase is not done until every box is ticked.
5. **Honour the CROSS-PHASE INVARIANTS** (see the dedicated section in
   [`01-phased-plan.md`](./01-phased-plan.md)): CLAUDE.md doc/error rules, 500/1500
   SLOC caps, **shared-crate publish-order + `cargo publish --dry-run` +
   whole-workspace `cargo check`**, **macOS FDA re-grant + cdhash verify after
   every daemon `cargo install` (#873)**, worktree discipline, and "the manifest is
   the only member registry after P0".

## Forced build order (why this sequence)

The order is dependency-driven, **revised by the adversarial review**:

1. **P0 — DOC-2 manifest first** (no upstream dep; kills the four-divergent-list
   hazard; unblocks `stack_version`, member selection, dispatch step 1).
   *Non-contract fields only* — contract fields land in P1 (two-pass, M2).
2. **P2 — DOC-4 rollup** (lattice + scope matrix). Completes the **Option-A
   deliverable**. Honors **DOC-4 RD1** (global `--timeout` only; per-member
   override deferred — B2) and **RD3** (a project-scope down/fail degrades only
   the project cell, does not move the exit — m6).
3. **DECISION GATE — Option A vs Option B** (D-OptAB).
4. *(Option B)* **P1 — DOC-1 contract + identity hoist.** The slug identity path
   is landed **INERT behind a flag** — `derive_index_id` (basename) stays the live
   key — because flipping it re-keys every index/palace and **orphans all state
   unless the re-key migration co-ships** (B1).
5. **P4a — trusty-search retrofit pulled FORWARD to gate P3** so dispatch is
   validated against a **real** member envelope, not a fixture (B3); A1/A2/A8 are
   pulled forward to P3.
6. **P3 — DOC-5 dispatch + passthrough + rewire `up/`** (enter-gated on DOC-12
   promotion, M5/D-M5).
7. **P4b/c/d — remaining retrofits**; **P5 — minimal mpm shim**.
8. **P6 — M15 re-key migration *and the slug-flag flip are ATOMIC*** — carried in
   trusty-search's `core::migration` framework with a `schema_version` bump
   (DOC-6 §7.1), re-keying by `root_path` (no reindex) (B1).
9. **P7a (harness code + A0–A8) / P7b (CI infra)** — split because the macOS-VM /
   Linux-container provisioning is a distinct, higher-risk concern (M4). A harness
   skeleton is pulled **early** (P0) because the 214 pure-function unit tests
   cannot catch dispatch/scope regressions.
10. **P8 — fast-follow** (transitive cluster fold C4 + DOC-11 residuals).

## Source of truth

The **02-design** set is the authoritative source for *intent* and
acceptance-criteria detail: [`../02-design/`](../02-design/) (DOC-0..DOC-12, with
ADR-0006/0007/0008/0011). DOC-7 is **superseded** (ADR-0011 / #1104 — its UI
surface moved to `trusty-console`); DOC-12 is **Draft** (its `up/` is the
most-built part of the crate and is subject to change). Where this plan states an
acceptance criterion, it cites the governing DOC section so it can be checked
against the design.
