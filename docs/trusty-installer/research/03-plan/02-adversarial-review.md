# 02 — Adversarial Review of the trusty-controller Completion Plan

> **Purpose.** A hard adversarial pass over the draft phased plan
> ([`01-phased-plan.md`](./01-phased-plan.md)) and its supporting snapshot
> ([`00-current-state.md`](./00-current-state.md)). Each finding below has a
> **Resolution in plan** line stating exactly how the revised plan addresses it
> — or that it is parked as an **OPEN DECISION** in the
> [`progress-manifest.md`](./progress-manifest.md) decisions log (no silent
> owner-call substitution).
>
> Severity tiers: **BLOCKER** (changes the phase graph), **MAJOR** (changes a
> phase's scope/gates), **MINOR** (corrects a detail or fills a gap).
>
> **Source of truth for intent:** [`../02-design/`](../02-design/) (DOC-0..DOC-12,
> ADRs). Where a finding cites a design fact (e.g. DOC-4 Resolved Decision 1,
> DOC-6 §7.1, DOC-12 §3, `index_id.rs:71`), that citation is the governing
> authority and overrides the draft's prose.

---

## TOP-5 (the findings that actually move the needle)

1. **B1 — The identity flip orphans all user state five phases before its
   migration exists.** This is the central defect: the draft hoists the
   canonical full-path-slug key in P1/P3 but only builds the re-key migration in
   P6. The live keying scheme is the **basename** (`derive_index_id`), not the
   slug. Flipping the key without the migration co-shipping orphans every index
   and palace. **Fix: the migration co-ships with the flip; until it lands, the
   slug path is INERT behind a flag and `derive_index_id` (basename) stays the
   live key.**
2. **B3 + M1 — P2/P3 exit gates pass on fixtures no real member emits.** No
   member emits `Envelope<T>`/`verbs[]`/`contract_version` today. Validating
   dispatch against synthetic fixtures violates the plan's own caveat (b). **Fix:
   pull the trusty-search retrofit forward to gate P3's exit, and pull harness
   assertions A1/A2/A8 forward to P3.**
3. **B2 — A timeout decision was inverted against an owner-approved resolution,
   citing a non-existent "M10".** DOC-4 RD1 (owner-approved 2026-06-08) *defers*
   per-member timeouts and ships a single global `--timeout` in v1. "M10" does
   not exist. **Fix: honor RD1; delete every "M10"; log the un-defer as an OPEN
   DECISION.**
4. **Whole-approach framing — P1+P4 (contract/dispatch rebuild) is being assumed
   as the spine, but by the plan's own caveat (a) it is worthless unless the
   cross-crate retrofit is funded in the same campaign.** **Fix: present Option A
   (value-first minimal sub-campaign) vs Option B (full rebuild) and add a
   DECISION GATE after P0/P2.**
5. **M3 + M4 — Cross-crate release reality and epic-sized phases are missing
   structure.** No publish-order/FDA/cdhash invariants; P4 and the harness phase
   are each really several independently-gated phases. **Fix: add a CROSS-PHASE
   INVARIANTS section; split P4 → P4a–P4d and the harness → code battery +
   CI-infra.**

---

## BLOCKERS (change the phase graph)

### B1 — Identity flip orphans state (the central defect)

**Finding.** The basename→full-path-slug canonical-id flip (ADR-0008 / DOC-3 §8)
re-keys **every** existing index and palace. Today the live key is the
**basename**: `trusty_common::derive_index_id` returns the basename
(`index_id.rs:71` doc: *"changing casing/punctuation would orphan every
previously-indexed project"*); `trusty-search/src/detect.rs` keys on the
basename; the slug form (`fs_discovery.rs::id_from_path`) is a **different live
key**. The draft hoists the slug walk in P1 and discovers/builds the M15 re-key
migration only in P6 — so P1/P3 would orphan all user state **five phases before
the migration exists**. The draft also mis-locates the migration as a
controller/common helper, and mis-labels the P1 risk as "subtly changes ids".

**Required fix (all parts mandatory):**

- **(a)** Restructure so the M15 re-key migration **co-ships** with the identity
  flip. Either merge the migration into the same phase that first writes the slug
  key, **or** keep `derive_index_id` (basename) as the live key and gate the
  canonical-slug path behind a flag that is **INERT until the migration lands**.
- **(b)** The migration MUST be carried in **trusty-search's existing
  `core::migration` `_meta` framework** (confirmed present: `m001..m004.rs`,
  idempotent/crash-safe) with a **`schema_version` bump**, per **DOC-6 §7.1** —
  NOT a controller/common helper as the draft P6 says. It re-keys **by
  `root_path`** with **no re-embed / no reindex**.
- **(c)** Re-label the P1 risk from "subtly changes ids" to **"flips the live
  keying scheme — orphans all state unless the migration co-ships."**

**Resolution in plan.** Adopted the **gated-inert** strategy:
- P1 now **explicitly keeps `derive_index_id` (basename) as the live key** and
  lands `id_from_path` (canonical slug) **behind an inert feature/flag** that no
  read/write path consults until the migration phase activates it. P1's risk
  bullet is rewritten verbatim per (c).
- The migration phase (renumbered **P6**) is rewritten so the migration lives in
  **trusty-search `core::migration` (`m005`-style) with a `schema_version`
  bump** per DOC-6 §7.1, re-keys by `root_path`, and **flips the inert flag as
  the last step** — i.e. the flip and the migration are now atomic within one
  phase.
- Caveat **(c)** in [`00-current-state.md`](./00-current-state.md) §8 and the
  phase-dependency diagram are updated to show the flag-flip/migration edge and
  to forbid any earlier phase from activating the slug key.

---

### B2 — Timeout decision inverted / invented "M10" citation

**Finding.** The draft says *"implement RD1 + M10 (un-deferred): per-member
timeout overrides ship day one."* This is wrong on two counts:
1. **DOC-4 Resolved Decision 1** (owner-approved **2026-06-08**) **defers**
   per-member manifest timeouts to a later release and ships **only a single
   global `--timeout` flag in v1**.
2. **"M10" does not exist** anywhere in DOC-4.

**Required fix:** remove every "M10" reference; default the plan to **HONOR RD1**
(global `--timeout` only in v1, per-member overrides deferred); add an OPEN
DECISION: *"Un-defer per-member timeouts? Requires explicit owner reversal of
DOC-4 RD1 — not assumed."*

**Resolution in plan.**
- Every "M10" token is deleted across all four plan files.
- P2 (rollup) WI/exit-gate/risk now specify **global `--timeout` > 2 s/10 s
  defaults** only; **per-member manifest `timeout` overrides are DEFERRED** per
  RD1. The manifest may *carry* a `timeout?` field for forward-compat, but the
  rollup **does not consult it in v1** (asserted negatively in the exit gate).
- Caveat **(d)** is rewritten to say "honor RD1 (global-only); per-member
  override is an OPEN DECISION."
- OPEN DECISION **D-B2** logged in the decisions log.

---

### B3 + M1 — Validation ordering (dispatch validated only against fixtures)

**Finding.** P2 (lattice/matrix) and P3 (dispatch/capability-negotiation)
consume `Envelope<T>` / `verbs[]` / `contract_version` that **no member emits
today** (zero hits across non-controller crates). As drafted, P2/P3 exit gates
pass on **synthetic fixtures with zero real-member coverage** — violating the
plan's own caveat (b) (*"unit tests won't catch dispatch/scope regressions"*).

**Required fix:**
- **(a)** Make the **trusty-search retrofit (first member, pattern-setter)** a
  **hard dependency that lands and gates P3's exit** — pull the search retrofit
  forward so it sits between the P3 dispatch skeleton and the P3 exit gate (or
  add to P3 EXIT GATE: *"dispatch validated against ≥1 real member emitting a
  real `Envelope<T>`, not a fixture"*).
- **(b)** Pull harness assertions **A1 (exit-code mirror), A2 (rollup schema),
  A8 (conformance self-check)** FORWARD to gate P3; leave the vanilla-env
  macOS-VM capstone at the late harness phase.
- **(c)** Stop labeling P2/P3 and P4 "parallelizable" — P2/P3 **acceptance**
  depends on the first real member.

**Resolution in plan.**
- **P4a (trusty-search retrofit)** is reordered to land **inside the P3 window**
  and is named a **hard gate on P3 exit**. P3 EXIT GATE adds: *"dispatch +
  rollup validated against trusty-search emitting a **real** `Envelope<T>` (P4a
  merged) — not a fixture."*
- Harness assertions **A1/A2/A8 are pulled forward** into P3's exit gate
  (asserted against the real search envelope). The vanilla-env macOS-VM capstone
  (A0/A3–A7 + cdhash) remains in the late harness phase (now **P7a/P7b**).
- The "two parallelizable tracks" note is **deleted**; replaced with an explicit
  statement that **P2/P3 acceptance depends on P4a (the first real member)**, so
  the consumer side cannot be treated as freely concurrent.

---

## MAJOR (change a phase's scope/gates)

### M2 — P0 wires contract fields before P1 finalizes them (two passes)

**Finding.** P0 collapses the 4 member lists and wires `contract_floor` before
P1 finalizes the contract fields → an unavoidable two-pass on the manifest.

**Required fix:** scope P0's manifest to **non-contract fields only**; add a P1
work item *"finalize manifest contract fields
(`min_contract_version`/`expected_contract_version`/floor) + re-verify the 4
collapsed call sites"*; state the two-pass explicitly.

**Resolution in plan.** P0 WI-1 now authors the manifest with **non-contract
fields only** (`id`, `display_name`, `binary`, `kind`, `install`, `version`,
`changelog`, `depends_on`, `enabled`, `stack_version`); `contract_floor`/
`min_contract_version`/`expected_contract_version` are **explicitly out of P0
scope**. P1 gains **WI "finalize manifest contract fields + re-verify the 4
collapsed call sites"**. Both P0 and P1 state the **two-pass** explicitly.

### M3 — Cross-crate release reality missing

**Finding.** No section covers publish order, the cargo-publish dry-run, macOS
FDA re-grant + cdhash verification after every daemon-binary `cargo install`
(CLAUDE.md #873), or `[patch.crates-io]` consistency for `trusty-common`.

**Required fix:** add a **CROSS-PHASE INVARIANTS** section referenced from every
phase that bumps `trusty-common`; add publish-order checks to P1/P4/P5 exit
gates.

**Resolution in plan.** A **CROSS-PHASE INVARIANTS** section is added to
[`01-phased-plan.md`](./01-phased-plan.md) (publish `trusty-common` **before**
consumers; cargo-publish skill + dry-run; re-grant FDA + verify cdhash after
every daemon-binary `cargo install`; keep `[patch.crates-io]` consistent). P1,
P4(a–d), and P5 exit gates each add a **publish-order check**.

### M4 — Epics sized as single phases

**Finding.** P4 (4 cross-crate retrofits) and the harness phase are each really
several independently-gated phases. Review implements **none** of the 7 verbs
(heaviest). GitHub-hosted macOS runners cannot trivially run nested VMs.

**Required fix:** promote P4 → **P4a(search)/P4b(memory)/P4c(analyze)/P4d(review)**
with independent enter/exit gates (review budgeted separately as heaviest). Split
the harness phase into **(i) harness CODE/battery** and **(ii) macOS-VM +
Linux-container CI INFRA**, separately gated; flag that GitHub-hosted macOS
runners cannot trivially run nested VMs (provisioning risk; the `macos-14`
runner itself is the isolation host per DOC-10 §3).

**Resolution in plan.** P4 is split into **P4a/P4b/P4c/P4d**, each with its own
enter/exit gate and manifest sub-row; **P4d (review)** carries a note that it
implements none of the 7 verbs and is budgeted as the long pole. The harness
phase is split into **P7a (harness code + A0–A8 battery)** and **P7b (CI infra:
macOS-VM/Linux-container + `isolation.yml`)**, separately gated. P7b records the
**nested-VM provisioning risk** and that the `macos-14` runner itself is the
isolation host (DOC-10 §3) — i.e. a clean `$HOME` on the runner, not a nested
VM, is the realistic v1 isolation.

### M5 — Building on DRAFT DOC-12

**Finding.** P3 rewires `up/` against DOC-12, which is still **Draft**.

**Required fix:** add a P3 ENTER GATE: *"DOC-12 promoted to Accepted (or owner
re-confirms Q1–Q6 in decisions log) before rewiring `up/`."*

**Resolution in plan.** P3 ENTER GATE adds the DOC-12-promotion precondition; an
OPEN DECISION pointer is noted so the owner either promotes DOC-12 or re-confirms
Q1–Q6 in the decisions log before P3's `up/` rewiring begins.

---

## MINOR (corrections / gap-fills)

### m1 — File paths wrong

**Finding.** `passthrough`/`probe`/`auto_update`/`stack`/`health`/`ensure`/
`mcp_patch` live under `src/commands/...` (e.g. `src/commands/passthrough.rs`,
`src/commands/stack/health.rs`, `src/commands/ensure/mcp_patch.rs`); `scope.rs`
**is** at `src/scope.rs` (correct).

**Resolution in plan.** Every BLAST RADIUS path and grep-based gate is corrected
to the `src/commands/...` prefix across all files; `src/scope.rs` left as-is.

### m2 — P0 collapse exit gate not binary

**Finding.** The collapse gate must be mechanically checkable.

**Required fix:** assert *"zero `vec![…]`/array literals of member ids remain in
`default_manifest`/`stable_set`/`analyze_core_targets`/`mcp_members`; these
symbols either deleted or return manifest queries"* — and note
`analyze_core_targets` is also read via `cfg.analyze_core_targets` in
`up/policy.rs` (enumerate allowed remaining refs).

**Resolution in plan.** P0 EXIT GATE rewritten to the binary form above, with an
explicit **allowed-remaining-refs** enumeration (the `up/policy.rs`
`cfg.analyze_core_targets` read is permitted as a manifest-backed query, not a
literal).

### m3 — `auto_update` fate

**Finding.** The draft says "delete unless grep finds a ref." That is an
owner-class call (delete the orphaned `maybe_notify` vs wire it to DOC-9 drift
notification).

**Resolution in plan.** Made an **OPEN DECISION (D-m3)** in the decisions log;
P0 no longer pre-decides deletion. Until the owner rules, `auto_update.rs` is
left in place with a TODO pointer to D-m3.

### m4 — Carried-but-unscheduled work has no home

**Required fix:** add a **"Carried-but-unscheduled / backlog"** section with
owners+phases for: DOC-8 §4 launch-hook (Claude Code SessionStart) verification;
`--latest`/BOM-drift consumer (`updates.rs:67` TODO #1316); upgrade rollback /
pin-back (`tctl upgrade --to`) testing (DOC-9); controller-side redaction
belt-and-suspenders + a CI negative-assertion that it fires when a member forgets
to redact (DOC-1 D8); golden-snapshot does **not** cover CLI arg-surface breaking
changes — add an arg-surface guard; observability/tracing for the dispatch path.

**Resolution in plan.** A **Carried-but-unscheduled / backlog** section is added
to [`01-phased-plan.md`](./01-phased-plan.md) with each item assigned a tentative
owner-phase, and the matching rows added to the progress manifest.

### m5 — Progress manifest honesty + 1:1 with work items

**Finding.** The update protocol is advisory/honor-system (unlike the mechanical
line-cap gate), and the checklist must match the restructured work items 1:1.

**Resolution in plan.** The manifest now **states plainly** that the update
protocol is **advisory/honor-system** and **proposes an optional lightweight CI
check** (a phase-labeled PR must move the manifest's phase status). The checklist
is rebuilt 1:1 against the restructured work items, including **P4a–P4d sub-rows
with per-crate restart-lifecycle testing** and the **P3 self-target/M5
reject-controller** line.

### m6 — P2 exit gate must assert both rollup invariants

**Required fix:** P2 exit gate must assert **both**: (i) stack aggregate
worst-wins `3≻1≻2≻0`, **and** (ii) per DOC-4 **RD3** a project-scope down/fail
degrades **only the project cell** and does **not** raise the process exit code.
The gate must **fail** an impl that lets project-pending/down move the exit.

**Resolution in plan.** P2 EXIT GATE now carries both assertions, with an
explicit negative test: a project-scope `down`/`fail` leaves the **process exit
code unchanged** (only the project cell degrades) per DOC-4 RD3.

---

## Whole-approach critique (strategic framing)

**Finding.** The draft treats the full contract/dispatch rebuild (P1/P3/P4/P5)
as the campaign spine. But the plan's own **caveat (a)** says the P1 contract
module is **worthless unless the P4 cross-crate retrofit is funded in the same
campaign** — so **P1+P4 must be justified together as a sub-campaign**, not
assumed as the spine. A large amount of real, low-risk value (the lying README,
the 4-list correctness hazard, the load-bearing *"unindexed project ≠ broken
stack"* rollup bug) can ship **without** any identity flip or cross-crate epic.

**The explicit strategic choice:**

- **Option A — value-first minimal sub-campaign.** Ship **P0** (manifest +
  README rewrite + collapse 4 lists + real `stack_version`) **and the P2
  lattice** as a **standalone deliverable**. Fixes the lying README, the 4-list
  correctness hazard, and the load-bearing *"unindexed project ≠ broken stack"*
  rollup bug — with **no identity flip** and **no cross-crate epic**.
- **Option B — full contract/dispatch rebuild.** P1/P3/P4(a–d)/P5 (+ P6
  migration, + P7 harness). Justified **only** as a P1+P4 sub-campaign per
  caveat (a).

**Resolution in plan.** [`README.md`](./README.md) and
[`01-phased-plan.md`](./01-phased-plan.md) now present Option A vs Option B
explicitly. A **DECISION GATE** is inserted **after P0/P2**: the campaign must
choose whether to proceed into Option B (committing P1+P4 *together*) or stop at
the Option-A deliverable. OPEN DECISION **D-OptAB** is logged.

---

## Findings index (traceability)

| ID | Severity | One-line | Resolution |
|---|---|---|---|
| B1 | BLOCKER | Identity flip orphans state before migration exists | Slug path inert behind flag; migration co-ships in P6 via trusty-search `core::migration` (DOC-6 §7.1) |
| B2 | BLOCKER | Timeout decision inverted; "M10" invented | Honor RD1 (global-only); "M10" deleted; OPEN DECISION D-B2 |
| B3+M1 | BLOCKER | Dispatch validated only on fixtures | P4a (search) gates P3 exit; A1/A2/A8 pulled forward; "parallelizable" removed |
| M2 | MAJOR | Manifest contract fields wired twice | P0 non-contract only; P1 finalizes contract fields (two-pass stated) |
| M3 | MAJOR | Cross-crate release reality missing | CROSS-PHASE INVARIANTS section; publish-order checks in P1/P4/P5 |
| M4 | MAJOR | Epics sized as phases | P4→P4a–d; harness→P7a (code) + P7b (CI infra); nested-VM risk flagged |
| M5 | MAJOR | Building on DRAFT DOC-12 | P3 ENTER GATE: DOC-12 Accepted or Q1–Q6 re-confirmed |
| m1 | MINOR | Wrong file paths | Corrected to `src/commands/...`; `src/scope.rs` kept |
| m2 | MINOR | P0 collapse gate not binary | Binary literal-free assertion + allowed-refs enumeration |
| m3 | MINOR | auto_update fate pre-decided | OPEN DECISION D-m3 |
| m4 | MINOR | Carried work unscheduled | Backlog section + manifest rows |
| m5 | MINOR | Manifest honesty + 1:1 | Advisory note + optional CI check; checklist rebuilt 1:1 |
| m6 | MINOR | P2 gate misses RD3 | Both worst-wins AND project-cell-only exit asserted |
| D-OptAB | DECISION | Option A vs B | Whole-approach choice; DECISION GATE after P0/P2 |

---

## OPEN DECISIONS raised by this review (see decisions log)

- **D-B2** — Un-defer per-member timeouts? (requires owner reversal of DOC-4 RD1).
- **D-m3** — `auto_update.rs::maybe_notify`: delete vs wire to DOC-9 drift notification.
- **D-OptAB** — Option A (value-first) vs Option B (full rebuild); decided at the
  DECISION GATE after P0/P2.
- **D-M5** — DOC-12 promotion to Accepted vs owner re-confirmation of Q1–Q6
  (gating P3 enter).
