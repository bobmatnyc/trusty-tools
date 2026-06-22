# Progress Manifest — trusty-controller / `tctl` completion campaign

> **LIVING DOCUMENT.** This is the single tracker for the campaign defined in
> [`01-phased-plan.md`](./01-phased-plan.md). It MUST be updated at every phase
> gate. Do not let it drift — a stale manifest is worse than none.
>
> Restructured after the adversarial pass
> ([`02-adversarial-review.md`](./02-adversarial-review.md)): the phase set is
> **P0, P2, [DECISION GATE], P1, P4a, P3, P4b/c/d, P5, P6, P7a, P7b, P8** (P2
> precedes P1 because P0+P2 is the value-first Option-A deliverable; P4a is pulled
> forward to gate P3).

---

## Update protocol (read before editing)

> 🟡 **HONESTY NOTE (m5).** This update protocol is **advisory / honor-system** —
> unlike the **mechanical** line-cap gate (`scripts/check_line_cap.sh`, CI-enforced),
> nothing forces this manifest to stay current. **Optional lightweight CI check
> (proposed):** a PR labeled with a phase (`phase:P3`, …) must change that phase's
> Status cell in this file, else CI warns/fails. Until that exists, keeping this
> file current is part of each phase's definition of done by convention only.

1. **At an ENTER gate:** before starting a phase, confirm every enter-gate
   precondition is met, set the phase **Status → IN PROGRESS**, set **Enter-gate
   met? → ✅** with the date, and add a Notes line.
2. **At an EXIT gate:** when all exit-gate criteria pass, set **Status →
   EXIT-GATE-PENDING** until independently verified (tests green, CI green), then
   **→ DONE** with **Exit-gate met? → ✅**, the date, and the merged **PR #(s)**.
3. **If blocked:** set **Status → BLOCKED**, record the blocker in Notes and in the
   "Deviations from plan" log.
4. **Every status change records:** the **date** (YYYY-MM-DD), the **PR #(s)**, and
   a one-line **Note**. Check the boxes in the per-phase checklist as work items
   merge.
5. **Decisions** (membership calls, deferrals, scope changes, OPEN DECISIONS) go in
   the **Decisions log**; **anything that departs from
   [`01-phased-plan.md`](./01-phased-plan.md)** goes in **Deviations from plan**.

**Status legend:** `NOT STARTED` · `IN PROGRESS` · `BLOCKED` ·
`EXIT-GATE-PENDING` · `DONE`.

---

## Phase status table

| Phase | Title | Status | Enter-gate met? | Exit-gate met? | PRs | Notes |
|---|---|---|---|---|---|---|
| **P0** | Manifest (non-contract) + repo hygiene + README | NOT STARTED | ☐ | ☐ | — | Keystone (DOC-2); contract fields deferred to P1 (M2) |
| **P2** | DOC-4 rollup: 4-value lattice + scope matrix | NOT STARTED | ☐ | ☐ | — | RD1 global-timeout-only; RD3 project-cell-only exit; defers C4→P8 |
| **DECISION GATE** | Option A vs Option B (after P0/P2) | NOT REACHED | — | — | — | Records D-OptAB; Option B requires P1+P4 joint funding |
| **P1** | Contract module (`trusty_common`) + identity hoist (INERT) + finalize contract fields | NOT STARTED | ☐ | ☐ | — | Option B only. Slug path INERT (B1); M2 second pass |
| **P4a** | trusty-search retrofit (pattern-setter) — GATES P3 | NOT STARTED | ☐ | ☐ | — | Pulled forward (B3); real Envelope<T> |
| **P3** | DOC-5 dispatch + real passthrough + rewire `up/` | NOT STARTED | ☐ | ☐ | — | Exit gated by P4a + A1/A2/A8 (B3); enter gated by DOC-12 (D-M5); closes TODO #920 |
| **P4b** | trusty-memory retrofit (T3) | NOT STARTED | ☐ | ☐ | — | Follows P4a pattern |
| **P4c** | trusty-analyze retrofit (T3) | NOT STARTED | ☐ | ☐ | — | Follows P4a pattern |
| **P4d** | trusty-review retrofit (T2, heaviest) | NOT STARTED | ☐ | ☐ | — | Implements none of 7 verbs; budget separately |
| **P5** | claude-mpm `uv` shim (minimal, T1) | NOT STARTED | ☐ | ☐ | — | Doomed when trusty-mpm lands |
| **P6** | M15 re-key migration + slug-flag FLIP (ATOMIC) | NOT STARTED | ☐ | ☐ | — | HIGHEST RISK; trusty-search core::migration + schema_version (DOC-6 §7.1); re-key by root_path |
| **P7a** | DOC-10 harness CODE + A0–A8 battery | NOT STARTED | ☐ | ☐ | — | A1/A2/A8 already proven in P3 |
| **P7b** | DOC-10 CI infra: macOS-VM/Linux-container + `isolation.yml` | NOT STARTED | ☐ | ☐ | — | Nested-VM risk; macos-14 runner is isolation host (DOC-10 §3) |
| **P8** | Cluster fold C4 + DOC-11 residuals | NOT STARTED | ☐ | ☐ | — | Fast-follow |

---

## Per-phase work-item checklist

### P0 — Manifest (non-contract) + repo hygiene
- [ ] Author `crates/trusty-controller/manifest.toml` — **NON-contract fields only** (M2): `id`/`display_name`/`binary`/`kind`/`install`/`version`/`changelog`/`depends_on`/`enabled`, `stack_version="YYYY.MM-N"`; review/tga/controller membership resolved; `timeout?` may exist but is unused (RD1)
- [ ] Build manifest loader `src/manifest/mod.rs` (`include_str!` embedded default + system override; precedence system > embedded)
- [ ] Wire the `--manifest <path>` flag (currently inert)
- [ ] Collapse all 4 hard-coded lists (`default_manifest`, `stable_set`, `analyze_core_targets`, `mcp_members`) → manifest queries; delete list constructors
- [ ] Replace `stack_version="0.0.0-scaffold"` with manifest value (leave `contract_floor` placeholder — finalized P1)
- [ ] `auto_update` fate = **OPEN DECISION D-m3** (do NOT delete; leave TODO); keep `src/scope.rs` (P3 marker)
- [ ] Rewrite stale README (remove "Phase 0 scaffold"/`publish=false`; add Option A vs B framing)
- [ ] Seed minimal harness skeleton (`tests/isolation_harness.rs` smoke + `isolation.yml` stub)
- [ ] Exit gate verified: **m2 binary check** (zero member-id `vec!`/array literals in the 4 symbols; only allowed ref = `cfg.analyze_core_targets` in `src/commands/up/policy.rs`, manifest-backed); real `stack_version`; tests/clippy/fmt/line-cap green

### P2 — DOC-4 rollup
- [ ] 4-value lattice `down≻degraded≻pending≻ready` (skipped absorbed)
- [ ] tools×scope matrix; system drives verdict; project `pending` never degrades global
- [ ] Timeouts: **global `--timeout` only** (2s/10s defaults) — **per-member manifest override DEFERRED (RD1, B2)**; synthesized `down` w/ `reason`
- [ ] `pending_since` time-escalation (§5.5)
- [ ] Exit-code aggregation worst-wins `3≻1≻2≻0`; **RD3: project-scope down/fail degrades only the project cell, does NOT raise process exit (m6)**; `--json` rollup struct
- [ ] Rewrite `src/commands/stack/health.rs` + `src/commands/stack/doctor.rs` onto the rollup (drop `.any()` fold)
- [ ] Regression test: healthy system + unindexed project → global `ready`, project cell `pending`
- [ ] Negative test (m6): project-scope down/fail leaves process exit unchanged; (B2) manifest `timeout?` NOT consulted
- [ ] Exit gate verified

### DECISION GATE — Option A vs Option B
- [ ] Record **D-OptAB** in decisions log (stop at Option A, OR proceed to Option B)
- [ ] If Option B: record **P1+P4 joint-funding commitment** (caveat a) and DOC-12-promotion plan (D-M5)

### P1 — Contract module + identity hoist (INERT) + finalize contract fields
- [ ] (Enter) DECISION GATE = "proceed to Option B" with P1+P4 joint funding
- [ ] Create `trusty_common::contract` `Envelope<T>` + status vocabularies + `Scope` enum
- [ ] `verbs[]` descriptor + `version --json` shape; enforce D3 versioning split
- [ ] `ContractTool` trait + `Dispatcher`
- [ ] `redact_value` + `***redacted***` marker (D8 pattern set)
- [ ] Golden-snapshot test C3 + seed `contract_version: 1` ledger
- [ ] Hoist `detect_project`/`id_from_path` (canonicalize-then-slug, ADR-0008) **behind an INERT flag**; `derive_index_id` (basename) STAYS the live key (B1)
- [ ] **Inert-flag test (B1):** no production read/write path consults `id_from_path`; live key is still basename
- [ ] Replace `tctl src/commands/probe.rs` `serde_json::Value` validation with `Envelope<T>`
- [ ] **Finalize manifest CONTRACT fields (M2 second pass):** `min_contract_version`/`expected_contract_version`/real `contract_floor`; re-verify the 4 collapsed call sites
- [ ] Exit gate verified: **publish-order (M3)** `cargo publish --dry-run -p trusty-common`; whole-workspace `cargo check`; snapshot fails on un-bumped mutation

### P4a — trusty-search retrofit (pattern-setter; GATES P3)
- [ ] Implement `ContractTool` on trusty-search; 7 verbs; `verbs[]`; `redact_value`; net-new `restart`
- [ ] `trusty-search version --json` snapshot test (verbs + `contract_version >= floor`)
- [ ] Restart lifecycle test (graceful drain #534; `health` before/after)
- [ ] Real `Envelope<T>` consumed by `tctl` without synthesis
- [ ] Exit gate verified: **publish-order + cdhash/FDA (M3)**; whole-workspace `cargo check`

### P3 — DOC-5 dispatch + real passthrough + rewire `up/`
- [ ] (Enter) **DOC-12 Accepted OR Q1–Q6 re-confirmed (D-M5)** before `up/` rewiring
- [ ] 9-step pipeline (load→select→scope→blast-gate→negotiate→invoke→collect→rollup→exit)
- [ ] Make `src/commands/passthrough.rs` real; first-class commands become sugar over it
- [ ] Reject self-target `tctl trusty-controller <verb>` (M5) → exit 3
- [ ] Thread scope; kill `src/main.rs` `let _ = cli.scope` (TODO #920); lifecycle verbs system-only
- [ ] Blast-radius gate (M5): non-TTY system-mutating w/o `--yes` → exit 3
- [ ] System advisory lock (M7) on the `ensure` rung
- [ ] Re-route `up/` through Dispatcher/passthrough (`src/commands/up/system_runner.rs`/`os_env.rs`); trust envelope status
- [ ] **Validated against real member (B3):** dispatch+rollup exercised against trusty-search (P4a) real `Envelope<T>` — not a fixture
- [ ] **A1 pulled forward:** `--json` `exit_code` == process exit
- [ ] **A2 pulled forward:** rollup matrix shape well-formed over real stack
- [ ] **A8 pulled forward:** `tctl doctor --self-check trusty-search` passes
- [ ] Exit gate verified

### P4b — trusty-memory retrofit
- [ ] `ContractTool`, 7 verbs, `verbs[]`, `redact_value`, net-new `restart`; follow P4a pattern
- [ ] version bump + `version --json` snapshot + redaction negative assertion + **restart-lifecycle test**
- [ ] `tctl doctor --self-check trusty-memory` passes; **publish-order + cdhash/FDA (M3)**
- [ ] Exit gate verified

### P4c — trusty-analyze retrofit
- [ ] `ContractTool`, 7 verbs, `verbs[]`, `redact_value`, net-new `restart`; follow P4a pattern
- [ ] version bump + `version --json` snapshot + redaction negative assertion + **restart-lifecycle test**
- [ ] `tctl doctor --self-check trusty-analyze` passes; **publish-order + cdhash/FDA (M3)**
- [ ] Exit gate verified

### P4d — trusty-review retrofit (heaviest)
- [ ] Full retrofit (implements **none** of the 7 today); `ContractTool`, 7 verbs, `verbs[]`, `redact_value`, `restart`
- [ ] version bump + `version --json` snapshot + redaction negative assertion + **restart-lifecycle test**
- [ ] `tctl doctor --self-check trusty-review` passes; **publish-order + cdhash/FDA (M3)**
- [ ] Exit gate verified

### P5 — claude-mpm `uv` shim
- [ ] `trusty_common::contract::orchestrator` shim (synthesize doctor/health/version; advertise exactly 3 verbs)
- [ ] Add orchestrator member to `manifest.toml` (`kind=orchestrator`, `install=python/uv`, version pin)
- [ ] Wire dispatch to route `kind=orchestrator` through the shim
- [ ] Graceful degrade + `https://astral.sh/uv` hint when uv absent
- [ ] Exit gate verified (self-check passes; advertises `["doctor","health","version"]`); **publish-order (M3)**

### P6 — M15 re-key migration + slug-flag FLIP (ATOMIC, isolated)
- [ ] Record backup/rollback plan in decisions log
- [ ] Add migration to **trusty-search `core::migration`** (m005-style `_meta` step + `schema_version` bump, DOC-6 §7.1) — NOT a controller/common helper (B1)
- [ ] Inventory all per-project keyed state (indexes, palaces, `.mcp.json`, controller state) keyed by OLD basename id
- [ ] Re-key by `root_path` (rename in place) — NO reindex/re-embed; idempotent
- [ ] `--dry-run` mode (no mutation)
- [ ] **Flip the inert slug flag ATOMICALLY** after re-key verifies (B1)
- [ ] Verification pass: zero old-basename-id stores remain; byte-count unchanged; pre=basename/post=slug test
- [ ] Rollback tested (restore from backup; schema_version downgrade behaviour defined)
- [ ] Exit gate verified; **publish-order + cdhash/FDA (M3)**

### P7a — DOC-10 harness CODE + A0–A8 battery
- [ ] Drive canonical scenario STEP 0–6 non-interactively
- [ ] A0 hard-dep guide-and-abort (exit 3 + rustup/uv hints)
- [ ] A1 exit-code mirror (generalize from P3)
- [ ] A2 rollup matrix shape (generalize from P3)
- [ ] A3 version truth post-install AND post-upgrade (asserted twice)
- [ ] A4 daemons running (`health` == running)
- [ ] A5 `.mcp.json` patched
- [ ] A6 idempotency (second run no-op)
- [ ] A7 progressive readiness non-racy (`ensure --wait` → `fresh`)
- [ ] A8 conformance self-check per member (+ mpm shim 3-verb) (generalize from P3)
- [ ] macOS cdhash/warm-boot assertion logic (M14, §3.4; no `cp`-over-PATH)
- [ ] Broken-member fault-injection makes harness FAIL (oracle proof), then reverted
- [ ] Exit gate verified (A0–A8 green locally)

### P7b — DOC-10 CI infra
- [ ] Vanilla-env construction (macOS: `macos-14` runner as isolation host — **document nested-VM limitation, M4**; Linux: container)
- [ ] `isolation.yml`: per-PR path-filtered macOS smoke + nightly full scenario + Linux legs
- [ ] macOS cdhash/warm-boot CI leg (install→launchd→health→restart→health; no `cp`)
- [ ] Exit gate verified (A0–A8 green on `macos-14` + Linux)

### P8 — Cluster fold C4 + DOC-11 residuals
- [ ] Transitive cluster fold over `depends_on ∪ deps[]`; populate `clusters[]`
- [ ] Cluster test (analyze depends_on search; search down → analyze in cluster)
- [ ] DOC-11 residual sweep: re-route stale DOC-7 refs → trusty-console
- [ ] README + abbreviation table re-audit (`tctl` alias present)
- [ ] Exit gate verified; campaign marked complete

---

## Backlog tracker (carried-but-unscheduled — M4)

> Mirror of [`01-phased-plan.md`](./01-phased-plan.md) "Carried-but-unscheduled".
> Promote into a phase when scheduled.

| # | Item | Owner-phase (tentative) | Status |
|---|---|---|---|
| BL-1 | DOC-8 §4 launch-hook (SessionStart) verification | P3 / P7a | ☐ |
| BL-2 | `--latest` / BOM-drift consumer (`updates.rs:67`, #1316) | post-P1 | ☐ |
| BL-3 | Upgrade rollback / pin-back (`tctl upgrade --to`) testing (DOC-9) | P7a | ☐ |
| BL-4 | Redaction belt-and-suspenders + CI negative-assertion (DOC-1 D8) | P1 + P4a–d | ☐ |
| BL-5 | Arg-surface guard (golden snapshot misses CLI arg-surface) | P1 | ☐ |
| BL-6 | Observability/tracing for the dispatch path (DOC-5) | P3 | ☐ |

---

## Decisions log

> Record membership calls, deferrals, scope changes, funding commitments, backup
> plans, and OPEN DECISIONS. Format:
> `YYYY-MM-DD — [Phase] decision — rationale — who`.

**OPEN DECISIONS (owner call required — do not assume a default):**

- **D-OptAB — [DECISION GATE] Option A (value-first; stop at P0+P2) vs Option B
  (full contract/dispatch rebuild).** Option B requires committing **P1 and P4
  together** (caveat (a) — the contract module is worthless without the cross-crate
  retrofit funded in the same campaign). **Status: OPEN** — decide at the DECISION
  GATE after P0/P2.
- **D-B2 — [P2] Un-defer per-member timeouts?** The plan honors **DOC-4 RD1**
  (owner-approved 2026-06-08): v1 ships **only a global `--timeout`**; per-member
  manifest overrides are **deferred**. Un-deferring requires an **explicit owner
  reversal of RD1** — NOT assumed. (The draft's "M10" does not exist.) **Status:
  OPEN.**
- **D-m3 — [P0] `auto_update.rs::maybe_notify` fate:** delete the orphan vs wire it
  to the DOC-9 drift notification. Owner call; not pre-decided. **Status: OPEN.**
- **D-M5 — [P3 enter] DOC-12 promotion:** promote DOC-12 to **Accepted**, OR have
  the owner **re-confirm Q1–Q6** in this log, before P3 rewires `up/`. **Status:
  OPEN.**

**Resolved / recorded decisions:**

- _(stub — first entry: P0 review/tga/controller membership resolution)_
- _(stub — DECISION GATE: D-OptAB outcome + P1+P4 joint-funding commitment if Option B)_
- _(stub — P6 backup/rollback plan before any state mutation)_

---

## Deviations from plan

> Anything that departs from [`01-phased-plan.md`](./01-phased-plan.md): reordered
> phases, dropped/added work items, changed acceptance criteria. Format:
> `YYYY-MM-DD — [Phase] deviation — reason — impact`.

- _(none yet)_

---

## Caveat watch (must stay satisfied through completion)

| # | Caveat | Owning phase(s) | Status |
|---|---|---|---|
| (a) | DOC-1 contract worthless without DOC-6 retrofit in same campaign → P1+P4 funded together | DECISION GATE + P1 + P4a–d | ☐ committed |
| (b) | Build DOC-10 harness EARLY; validate dispatch against a REAL member (B3) | P0 skeleton → P4a gates P3 → P7a/P7b | ☐ |
| (c) | Identity flip orphans ALL state unless re-key migration co-ships; slug path INERT until P6 (B1) | P1 (inert) + P6 (atomic flip) | ☐ |
| (d) | Honor DOC-4 RD1 (global `--timeout` only); per-member override deferred (B2) | P2 + D-B2 | ☐ |
| (e) | DOC-11 still routes UI follow-ups to superseded DOC-7 → trusty-console | P8 | ☐ |
| (f) | DOC-12 is DRAFT while its `up/` is most-built → P3 enter gated on promotion (M5/D-M5) | P3 | ☐ |
