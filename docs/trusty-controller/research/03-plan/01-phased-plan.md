# 01 — Phased Implementation Plan (trusty-controller / `tctl`)

> **Source of truth for *intent*:** [`../02-design/`](../02-design/) (DOC-0..DOC-12).
> **Source of truth for *as-built*:** [`00-current-state.md`](./00-current-state.md).
> **Adversarial review that shaped this plan:** [`02-adversarial-review.md`](./02-adversarial-review.md).
> **Living tracker (update at every gate):** [`progress-manifest.md`](./progress-manifest.md).
>
> This plan defines phases in this **execution order**: **P0 → P2 → [DECISION
> GATE] → P1 → P4a → P3 → P4b → P4c → P4d → P5 → P6 → P7a → P7b → P8.** (The
> numbering is deliberately *dependency-ordered*, not alphabetical — P2 precedes
> P1 because the Option-A deliverable, P0+P2, must be shippable **without** the
> contract rebuild; P4a is pulled forward to gate P3.) Each phase has an **ENTER GATE**
> (preconditions), ordered **WORK ITEMS** (each PR-sized), an **EXIT GATE** of
> binary pass/fail acceptance criteria, **RISKS + MITIGATIONS**, and a **BLAST
> RADIUS** (crates/files touched). Acceptance criteria are drawn from the
> referenced DOC sections so they are verifiable, not aspirational.

---

## Strategic choice — Option A vs Option B (read first)

The adversarial review (whole-approach critique) surfaced that the full
contract/dispatch rebuild is **not** an unconditional spine. By the plan's own
caveat (a), the P1 contract module is **worthless unless the P4 cross-crate
retrofit is funded in the same campaign**. So the campaign must make an explicit
choice:

- **Option A — value-first minimal sub-campaign.** Ship **P0** (manifest +
  README rewrite + collapse the 4 lists + real `stack_version`) **and the P2
  lattice** as a **standalone deliverable**. This alone fixes:
  - the **lying README** (claims scaffold / `publish = false`),
  - the **four-divergent-member-list** correctness hazard, and
  - the load-bearing **"unindexed project ≠ broken stack"** rollup bug.
  Option A involves **no identity flip** and **no cross-crate epic** — it is
  low-risk, self-contained, and independently valuable.

- **Option B — full contract/dispatch rebuild.** P1 / P3 / P4a–P4d / P5, plus
  the P6 migration and the P7a/P7b harness. Per caveat (a), **P1 and P4 must be
  justified and funded *together*** — committing to P1 without funding P4 ships a
  contract module no member emits.

A **DECISION GATE** sits **after P0/P2** (see the diagram). The campaign decides
there whether to stop at the Option-A deliverable or commit to Option B as a
P1+P4 sub-campaign. The decision is logged as **D-OptAB** in the decisions log.

---

## Phase dependency / ordering diagram

```
            ┌──────────────────────────────────────────────────────────────┐
            │ P0  manifest.toml + loader (NON-contract fields) · collapse 4 │
            │     lists · real stack_version · README rewrite · MIN harness  │
            │     skeleton   (auto_update fate = OPEN DECISION D-m3)          │
            └───────────────┬──────────────────────────────────────────────┘
                            │ (DOC-2 manifest is the keystone — no upstream dep)
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P2  DOC-4 rollup: 4-value lattice + tools×scope matrix        │
            │     global --timeout only (RD1) · RD3 project-cell-only exit   │
            │     (DEFER transitive cluster fold C4 → P8)                    │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
   ╔════════════════════════════════════════════════════════════════════════╗
   ║  DECISION GATE — Option A (stop here, value-first) vs Option B (rebuild) ║
   ║  Option B requires committing P1 + P4 TOGETHER (caveat (a)).  [D-OptAB]  ║
   ╚════════════════════════════════════════════════════════════════════════╝
                            │ (Option B only)
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P1  trusty_common::contract (Envelope<T>, verbs[], ContractTool│
            │     Dispatcher) + golden snapshot + redaction                  │
            │     + HOIST identity walk — slug path INERT behind flag (B1)   │
            │     + finalize manifest CONTRACT fields (M2 second pass)       │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P4a trusty-search retrofit (pattern-setter) — emits a REAL     │
            │     Envelope<T>.  HARD-GATES P3 exit (B3).                     │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P3  DOC-5 9-step dispatch + REAL passthrough + scope (kill     │
            │     TODO #920) + blast-gate (M5) + rewire up/                  │
            │     EXIT gated by P4a (real member) + A1/A2/A8 (B3)            │
            │     ENTER gated by DOC-12 Accepted/Q1–Q6 re-confirmed (M5)     │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P4b trusty-memory · P4c trusty-analyze · P4d trusty-review     │
            │     (each independently gated; review heaviest — none of 7)    │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P5  claude-mpm uv shim (minimal, T1 only)                      │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P6  M15 re-key migration + FLIP the inert slug flag (ATOMIC).  │
            │     Carried in trusty-search core::migration (schema_version   │
            │     bump, DOC-6 §7.1); re-key by root_path, NO reindex. (B1)   │
            └───────────────┬──────────────────────────────────────────────┘
                            ▼
            ┌───────────────────────────────┐   ┌──────────────────────────┐
            │ P7a harness CODE + A0–A8       │   │ P7b CI INFRA: macOS-VM / │
            │     battery (code only)        │──▶│ Linux-container +        │
            │                                │   │ isolation.yml (M4)       │
            └───────────────┬───────────────┘   └────────────┬─────────────┘
                            └───────────────┬─────────────────┘
                                            ▼
            ┌──────────────────────────────────────────────────────────────┐
            │ P8  fast-follow: transitive cluster fold (C4)                  │
            │     + DOC-11 residual follow-ups (re-route stale DOC-7 refs)   │
            └──────────────────────────────────────────────────────────────┘
```

**Ordering rationale.** DOC-2 (manifest, **P0**) is built **first**: no upstream
dependency, kills the four-divergent-list hazard, unblocks `stack_version`,
member selection, and dispatch step 1. The DOC-4 rollup (**P2**) lands next so
the Option-A deliverable (P0+P2) is independently shippable. **Then the DECISION
GATE.** In Option B, DOC-1 (**P1**) lands the contract + the *inert* identity
walk; **P4a (search retrofit) is pulled forward to gate P3** so dispatch is
validated against a **real** member (not a fixture); the rest of the retrofit
epic (P4b–P4d) follows; the shim (P5) is minimal/last; the **M15 migration and
the slug-flag flip are atomic in P6**; the harness splits into **code (P7a)** and
**CI infra (P7b)**; the speculative cluster fold (C4) and DOC-11 residuals are
the fast-follow (P8).

> **There is no "freely parallelizable" consumer track.** (The draft's
> "two parallelizable tracks" note is removed.) **P2/P3 acceptance depends on
> P4a** (the first real member) — the controller side cannot be validated until a
> real member emits an `Envelope<T>`. P4b–P4d may proceed concurrently *after*
> P4a sets the pattern, but P3's exit gate is the synchronization point.

---

## CROSS-PHASE INVARIANTS (every phase must hold)

> Referenced from every phase below that bumps `trusty-common` or installs a
> daemon binary. (M3.)

- **CLAUDE.md rules:** Why/What/Test doc comments on public items; `thiserror` in
  libraries (`trusty-common`), `anyhow` in binaries (`tctl`); no `unwrap()` in
  library code; logs to **stderr** (stdout is contract-JSON only).
- **SLOC caps:** 500 prod / 1500 test; run `bash scripts/check_line_cap.sh` before
  every commit; split modules proactively.
- **Shared-crate publish order (M3):** when a phase bumps `trusty-common`
  (P1, P5, P6), **publish `trusty-common` BEFORE its consumers**. Follow the
  cargo-publish skill with a **`cargo publish --dry-run`** first; bump the
  dependent-crate pins and run **whole-workspace `cargo check`** + each
  dependent's tests. Keep `[patch.crates-io]` for `trusty-common` consistent in
  the root `Cargo.toml`.
- **macOS daemon-install hygiene (M3, CLAUDE.md #873):** after **every**
  `cargo install` of a daemon binary (search/memory/analyze/controller),
  **re-grant Full Disk Access** and **verify the cdhash** (the install must use
  `cargo install`'s atomic rename — **never `cp`-over-PATH**). Restart via
  `launchctl bootout`→`bootstrap` (SIGTERM), not `kickstart -k`.
- **Worktree discipline:** all work in a dedicated worktree off `origin/main`; the
  main checkout is inspection-only.
- **Manifest is the only registry:** after P0, **no** hard-coded member list may be
  re-introduced in any phase.
- **Update the progress manifest at every enter/exit gate** (date, PR #, status).
  Note this protocol is **advisory/honor-system** (see manifest m5).

---

## P0 — Manifest + repo hygiene (DOC-2; DOC-0 README; cleanup)

**Goal (one line):** make a single embedded `manifest.toml` the sole source of the
member list (non-contract fields), and clean up the repo's stale/dead surface.

**Satisfies:** DOC-2 (manifest/BOM, `stack_version`), DOC-0 (README/charter),
repo hygiene, partial DOC-10 (harness skeleton seed).

### ENTER GATE
- Worktree provisioned off `origin/main` (campaign's first phase).
- [`00-current-state.md`](./00-current-state.md) reviewed; four divergent member
  lists located (`src/commands/up/manifest.rs::default_manifest`,
  `src/commands/stable_set.rs`, `src/commands/up/manifest.rs::analyze_core_targets`,
  `src/commands/ensure/mcp_patch.rs::mcp_members`).

### WORK ITEMS (each ≈ one PR)
1. **Author `crates/trusty-controller/manifest.toml` — NON-contract fields only
   (M2).** One `[[member]]` per member with the DOC-2 §3 schema for the
   *non-contract* fields: `id`, `display_name`, `binary`,
   `kind` (`daemon|cli|controller|orchestrator`),
   `install = { source = "cargo", crate = … }` (or `python`/`uv` for the
   orchestrator), `version` pin, `changelog`, `depends_on?`, `enabled`. Set
   `stack_version = "YYYY.MM-N"`. A `timeout?` field MAY be carried for
   forward-compat but is **not consumed in v1** (RD1, B2). **The contract fields
   (`min_contract_version`, `expected_contract_version`, `contract_floor`) are
   OUT OF P0 SCOPE — finalized in P1 (second pass).** Reconcile the
   **review/tga/controller** disagreements explicitly (decide membership; record
   in the decisions log).
2. **Build the manifest loader** (`src/manifest/mod.rs`): `include_str!` the
   embedded default; resolve the optional system override
   (`~/.config/trusty-controller/manifest.toml` via `trusty_common` config
   helpers); precedence **system override > embedded default**; wire the
   `--manifest <path>` flag (currently inert) to override both.
3. **Collapse the four lists (m2 binary gate).** Replace every one of the four
   hard-coded lists with a query against the loaded manifest. Delete the dead list
   constructors. (Allowed remaining ref: `cfg.analyze_core_targets` read in
   `src/commands/up/policy.rs`, now backed by a manifest query — see EXIT GATE.)
4. **Wire real `stack_version`** — replace `src/commands/version.rs` /
   `src/commands/output.rs` `"0.0.0-scaffold"` with the manifest `stack_version`.
   **Leave `contract_floor` as-is for now (a literal/placeholder); it is finalized
   in P1** — state this two-pass explicitly in the PR description.
5. **Dead-code: `auto_update` fate is an OPEN DECISION (D-m3).** Do **not** delete
   in P0. Leave `src/commands/auto_update.rs::maybe_notify` in place with a TODO
   pointing at D-m3 (delete the orphan vs wire to DOC-9 drift notification — owner
   call). **Keep `src/scope.rs`** with a "wired in P3" marker.
6. **Rewrite the stale README** — remove "Phase 0 scaffold" / `publish = false`
   language; describe the real shipped surface (`install`/`upgrade`/`ensure`/
   `start`/`stop`/`restart`/`config`/`port`/`doctor --self-check`/`ui`/`up`),
   present the **Option A vs Option B** framing, and point to the design set.
7. **Seed a minimal harness skeleton** — create
   `crates/trusty-controller/tests/isolation_harness.rs` with a single smoke
   assertion (`tctl version --json` parses + `stack_version` is non-placeholder)
   and a stub `.github/workflows/isolation.yml` CI job. (Full battery/CI in
   P7a/P7b.)

### EXIT GATE (binary pass/fail)
- [ ] `crates/trusty-controller/manifest.toml` exists, is `include_str!`-embedded,
      and parses into the `[[member]]` schema (unit test asserts every required
      **non-contract** field present per member).
- [ ] **Collapse is mechanically clean (m2):** a grep proves **zero `vec![…]` /
      array literals of member ids** remain in `default_manifest`, `stable_set`,
      `analyze_core_targets`, `mcp_members` — these symbols are **deleted or return
      manifest queries**. The **only** allowed remaining reference is
      `cfg.analyze_core_targets` in `src/commands/up/policy.rs`, and a test asserts
      it resolves from the manifest (not a literal).
- [ ] `tctl version --json` emits a real `stack_version` (matches the manifest;
      **not** `0.0.0-scaffold`). (`contract_floor` may still be a placeholder here;
      finalized in P1.)
- [ ] `--manifest <path>` override is honoured (test: a temp override file changes
      the loaded member set).
- [ ] README contains **no** "Phase 0 scaffold" / `publish = false` text and
      presents the Option A / Option B choice.
- [ ] `auto_update.rs::maybe_notify` **retained** with a D-m3 TODO (not deleted);
      `src/scope.rs` retained with a P3 marker.
- [ ] `cargo test -p trusty-controller` green; `cargo clippy -p trusty-controller
      -- -D warnings` clean; `cargo fmt --check` clean.
- [ ] `bash scripts/check_line_cap.sh` clean (manifest loader split if >500 SLOC).
- [ ] Harness skeleton smoke test runs (locally; CI stub present).
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated (date, PR #s).

### RISKS + MITIGATIONS
- **R: choosing wrong membership for review/tga/controller.** → Record the decision
  + rationale in the decisions log; the manifest is a single editable file, so a
  wrong call is a one-line fix later.
- **R: `up/` already consumes the hard-coded lists and breaks.** → P0 only swaps
  the *list source*, preserving member identities; full `up/` rewiring is deferred
  to P3. Add a regression test that `up/`'s member set equals the manifest set.

### BLAST RADIUS
`crates/trusty-controller/`: new `manifest.toml`, new `src/manifest/` module,
`src/commands/up/manifest.rs`, `src/commands/stable_set.rs`,
`src/commands/ensure/mcp_patch.rs`, `src/commands/up/policy.rs`,
`src/commands/version.rs`, `src/commands/output.rs`, `src/main.rs` (`--manifest`
wiring), `README.md`, new `tests/isolation_harness.rs`, new
`.github/workflows/isolation.yml` (stub). Possibly `trusty-common` config helpers
(read-only use).

---

## P2 — DOC-4 rollup: 4-value lattice + tools×scope matrix

**Goal (one line):** replace the flat 2-value fold with the 4-value verdict lattice
and the tools×scope matrix that gets *"unindexed project ≠ broken stack"* right.

**Satisfies:** DOC-4 §2 (lattice), §2.0 (vocabulary), §1.3 + **RD1** (global
`--timeout` only in v1), **RD3** (project-cell-only exit), §7 (exit codes).
**Defers** the transitive cluster fold (C4) to P8.

> P2 lands directly after P0 (it consumes the manifest's `depends_on`/`kind`, not
> the contract module). It completes the **Option-A deliverable**. It does **not**
> require the typed `Envelope<T>` from P1 — it can parse member output via the
> existing `src/commands/probe.rs` path and switch to `Envelope<T>` opportunistically
> once P1 lands.

### ENTER GATE
- P0 EXIT GATE met (manifest is the sole member source; `stack_version` real;
  `depends_on` available from the manifest).

### WORK ITEMS (each ≈ one PR)
1. **Implement the 4-value lattice** `down ≻ degraded ≻ pending ≻ ready`, with
   `skipped` **absorbed** (neither improves nor worsens). Map both source
   vocabularies (doctor, health) into it per DOC-4 §2.0.
2. **Tools×scope matrix** — each member yields `cells.{system,project}`. **System
   track (GLOBAL) drives the stack verdict; a project `pending` (LOCAL) never
   degrades the global verdict.** Unit-test this rule explicitly (it is the bug the
   current flat fold has).
3. **Timeouts — global `--timeout` only (RD1, B2).** 2 s health / 10 s doctor
   defaults, overridable by a **single global `--timeout` flag**. **Per-member
   manifest timeout overrides are DEFERRED** (DOC-4 RD1, owner-approved
   2026-06-08). The rollup **does not read the manifest `timeout?` field in v1**
   (asserted negatively in the exit gate). On timeout/spawn-fail/unparseable,
   synthesize a `down` envelope stamped with a `reason`
   (`not_running|timeout|wedged|unreachable|contract_incompatible`).
4. **`pending_since` time-escalation** — a `pending` check stalled past the
   staleness budget rolls up to `degraded` (§5.5).
5. **Exit-code aggregation (worst-wins + RD3, m6).** Stack aggregate = worst-wins
   `3 ≻ 1 ≻ 2 ≻ 0` driven by the **system track**; a **project-scope `down`/`fail`
   degrades ONLY the project cell and does NOT raise the process exit code**
   (DOC-4 RD3). `--json` rollup struct emits `verdict`, `exit_code`, `summary`
   counts (incl. `na`), `cells`, `clusters[]` (empty until P8), `remediations[]`.
6. **Rewrite `src/commands/stack/health.rs` + `src/commands/stack/doctor.rs`** to
   consume the new rollup (replace the `.any()` fold). Parallel collection via
   bounded `tokio` join set.

### EXIT GATE (binary pass/fail)
- [ ] `tctl stack doctor --json` emits a `verdict ∈ {ready,degraded,pending,down}`
      (4-value), **not** a 2-value boolean.
- [ ] Unit test: a healthy system + an **unindexed project** rolls up to a
      **global `ready`** with a **project cell `pending`**; a regression test
      asserts the global verdict is *not* `degraded`/`down`.
- [ ] **RD3 (m6):** a project-scope `down`/`fail` degrades **only the project
      cell** and the **process exit code is unchanged** (negative test must FAIL an
      impl that lets project-pending/down move the exit).
- [ ] `summary` counts sum to the member count including `na`; matrix has
      `cells.{system,project}` for every member (DOC-4 §8.2 A2 shape).
- [ ] A hung member is synthesized as `down` with a `reason`, and the rest of the
      matrix still renders (no partial blocking). **Global `--timeout` is honoured;
      the manifest `timeout?` field is NOT consulted (RD1 — negative assertion).**
- [ ] Stack exit code follows worst-wins `3 ≻ 1 ≻ 2 ≻ 0`; the `--json` `exit_code`
      field mirrors the process exit (A1 mirror).
- [ ] `pending_since` escalation test: a `pending` past budget → `degraded`.
- [ ] `cargo test -p trusty-controller` green; clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated.

### RISKS + MITIGATIONS
- **R: re-introducing the flat-fold bug under a new name.** → The explicit
  "unindexed project ≠ broken" regression test is the gate; it must exist before
  the phase closes.
- **R: drifting from RD1 by quietly honouring per-member timeouts (B2).** → The
  exit gate's **negative** assertion (manifest `timeout?` not consulted) is the
  guard; un-deferring requires owner reversal (OPEN DECISION D-B2).
- **R: cluster fold (C4) entanglement.** → Keep `clusters[]` an empty-but-valid
  array in P2; the lattice + scope split is load-bearing, the cluster fold is
  speculative at 7 members → P8.

### BLAST RADIUS
`crates/trusty-controller/`: `src/commands/stack/health.rs`,
`src/commands/stack/doctor.rs`, new `src/rollup/` module (lattice, matrix,
exit-codes), `src/commands/probe.rs` (timeout/synthesis),
`src/commands/output.rs` (`--json` rollup struct). Reads manifest
`depends_on`/`kind`.

---

## ── DECISION GATE — Option A vs Option B (after P0/P2) ──

**This is a gate, not a phase.** Before any P1/P3/P4 work begins, the campaign
records **D-OptAB** in the decisions log:

- **Stop at Option A** — ship P0+P2 as the value-first deliverable; close the
  campaign (or pause it) here. No identity flip, no contract module, no
  cross-crate epic.
- **Proceed to Option B** — commit to funding **P1 and P4 together** (caveat (a)):
  the contract module is only worthwhile if the cross-crate retrofit lands in the
  same campaign. Record the funding commitment and the DOC-12-promotion plan
  (D-M5) here.

Do not enter P1 until D-OptAB records "proceed to Option B" **with** the P1+P4
joint-funding commitment.

---

## P1 — Contract module in `trusty_common` (DOC-1; identity hoist, INERT)

**Goal (one line):** land the shared `trusty_common::contract` module + the hoisted
project-identity walk (slug path **inert behind a flag**), and finalize the
manifest's contract fields.

**Satisfies:** DOC-1 D1–D8 (ADR-0007), DOC-3 §8 identity hoist (ADR-0008, gated),
implementation-spine steps 1–2, and the M2 second pass.

### ENTER GATE
- DECISION GATE recorded "proceed to Option B" **with P1+P4 joint-funding
  commitment** (caveat (a), D-OptAB).
- P0 EXIT GATE met (manifest is the sole member source; `stack_version` real).

### WORK ITEMS (each ≈ one PR)
1. **Create `crates/trusty-common/src/contract/mod.rs`** — serde `Envelope<T>`
   (`contract_version`, `tool`, `tool_version`, `verb`, `scope`, `status`, `data`,
   `messages`), the status vocabularies (doctor `ok|warn|fail|pending|skipped`,
   health `running|degraded|down`), and the `Scope` wire enum
   (`project|system|all`).
2. **`verbs[]` capability descriptor** + the `version --json` shape
   (`{tool,version,contract_version,verbs[]}`). Enforce DOC-1 D3 "versioning
   split": verb presence is independent of `contract_version`.
3. **`ContractTool` trait + `Dispatcher`** (D6) — the trait each Rust tool
   implements; the dispatcher the controller shares.
4. **`redact_value` helper + `***redacted***` marker** (D8), with the
   high-confidence pattern set (AKIA…, Bearer/Authorization, `scheme://user:pass@`,
   `sk-`/`ghp_`/`xox…`, long high-entropy blobs).
5. **Golden-snapshot test (C3)** — serialize the envelope + each verb's `data`
   shape; a snapshot diff fails CI unless `contract_version` is bumped **and** a
   "what's new per contract_version" ledger row is added. Seed the ledger with
   `contract_version: 1`. **Note (m4 backlog):** the golden snapshot does **not**
   cover CLI arg-surface breaking changes — an arg-surface guard is tracked in the
   backlog.
6. **Hoist the identity walk — but keep the slug path INERT (B1).** Promote
   `detect_project` / `id_from_path` (full-path-slug of nearest git root,
   canonicalize-then-slug, ADR-0008) into `trusty_common` next to the
   already-hoisted `slugify_string`. **`trusty_common::derive_index_id`
   (BASENAME) REMAINS the live key.** The slug `id_from_path` is added **behind a
   feature/flag that no read or write path consults** until the P6 migration flips
   it. Fallback to cwd-path-slug with a warning when no git root/marker.
7. **Replace `tctl`'s `src/commands/probe.rs`** ad-hoc `serde_json::Value`
   validation with the typed `Envelope<T>` parse from the new module.
8. **Finalize manifest CONTRACT fields (M2 second pass).** Add
   `min_contract_version` / `expected_contract_version?` per member and replace the
   P0 placeholder `contract_floor` with the DOC-1 D2 value. **Re-verify the 4
   collapsed call sites** still resolve correctly now that the contract fields are
   present.

### EXIT GATE (binary pass/fail)
- [ ] `trusty_common::contract` compiles and is re-exported; `Envelope<T>`,
      `verbs[]`, `ContractTool`, `Dispatcher`, `redact_value` are public.
- [ ] Golden-snapshot test **green**; deliberately mutating a `data` field
      **without** a `contract_version` bump makes it **fail** (proven by a
      throwaway local edit, then reverted).
- [ ] `***redacted***` redaction unit tests pass for every pattern in the D8 set.
- [ ] `detect_project`/`id_from_path` live in `trusty_common`; a unit test proves
      the same git checkout yields the same **slug** id from any subdirectory
      (canonicalize-then-slug), and the no-root fallback emits a warning. **A test
      asserts the slug path is INERT: `derive_index_id` (basename) is still the key
      every live read/write uses** (B1) — no production read/write path consults
      `id_from_path` yet.
- [ ] `tctl probe.rs` parses real envelopes via `Envelope<T>` (no
      `serde_json::Value` envelope validation remains).
- [ ] Manifest now carries `min_contract_version`/`expected_contract_version`/real
      `contract_floor`; `tctl version --json` emits the real floor; the 4 collapsed
      call sites re-verified (M2).
- [ ] **Publish-order check (M3):** `cargo publish --dry-run -p trusty-common`
      passes; consumer pins bumped; **whole-workspace `cargo check`** green;
      `cargo test -p trusty-common` + `-p trusty-controller` green; clippy/fmt clean;
      line-cap clean (split `contract/` into submodules if >500).
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated.

### RISKS + MITIGATIONS
- **R: contract module ships but no member emits envelopes (caveat (a)).** → ENTER
  GATE requires the P1+P4 joint-funding commitment; P1's value is realized only
  when P4 lands. Keep the module minimal (`contract_version: 1`,
  additive-superset) to avoid churn before consumers exist.
- **R: `trusty_common` is a shared crate — a change ripples to all dependents.** →
  Run full-workspace `cargo check` + dependent-crate tests (cross-phase invariant);
  publish `trusty-common` before consumers; bump + propagate pins.
- **R (B1, rewritten): the hoist FLIPS THE LIVE KEYING SCHEME — orphans all state
  unless the migration co-ships.** The basename (`derive_index_id`) is the live
  key; the slug (`id_from_path`) is a *different* key. → P1 keeps the slug path
  **inert behind a flag**; the flip happens **only in P6, atomically with the
  re-key migration**. The inert-flag test (above) is the gate that prevents an
  accidental early flip.

### BLAST RADIUS
`crates/trusty-common/` (new `src/contract/` module, identity walk + inert slug
flag, version bump); `crates/trusty-controller/` (`src/commands/probe.rs`,
`src/commands/version.rs` floor, `manifest.toml` contract fields); every crate that
depends on `trusty-common` (compile-check only at this phase).

---

## P4a — trusty-search retrofit (DOC-6 §2, pattern-setter) — GATES P3

**Goal (one line):** make trusty-search emit the DOC-1 envelope + advertise
`verbs[]` so the controller's dispatch (P3) is validated against a **real** member,
not a fixture (B3).

**Satisfies:** DOC-6 §2 conformance retrofit (T3 pattern-setter); closes the
fixture-only gap in caveat (b)/B3.

> **Pulled forward (B3).** P4a lands **inside the P3 window** and is a **hard gate
> on P3's exit** — dispatch + rollup must be exercised against trusty-search
> emitting a real `Envelope<T>`. It also sets the retrofit pattern that P4b–P4d
> copy.

### ENTER GATE
- P1 EXIT GATE met (`trusty_common::contract` + `ContractTool` available; floor
  finalized).

### WORK ITEMS (each ≈ one PR)
1. **Implement `ContractTool` on trusty-search** — emit the envelope for all 7
   verbs (`doctor health version start stop restart config`); advertise `verbs[]`;
   route secrets through `redact_value`; add net-new `restart`. This crate defines
   the retrofit pattern the others copy.
2. **`<binary> version --json` snapshot test** — advertises `verbs[]` + a
   `contract_version >= floor`.
3. **Restart lifecycle test** — reuse the graceful-drain shutdown (#534) +
   launchd `bootout`/`bootstrap`; verify `health` before/after restart.

### EXIT GATE (binary pass/fail)
- [ ] `trusty-search version --json` advertises `verbs[]` containing the 7 verbs
      and a `contract_version >= floor`.
- [ ] trusty-search emits a typed `Envelope<T>` for `doctor`/`health` consumed by
      `tctl` without synthesis.
- [ ] Secrets redacted with `***redacted***` (negative conformance assertion,
      DOC-6 §8).
- [ ] Restart lifecycle test green (graceful drain; `health` running before/after).
- [ ] **Publish-order check (M3):** if `trusty-common` was bumped, it is published
      first; cdhash re-verified + FDA re-granted after `cargo install trusty-search`.
- [ ] `cargo check` whole-workspace green; `cargo test -p trusty-search` green;
      clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) P4a row updated.

### RISKS + MITIGATIONS
- **R: net-new `restart` introduces lifecycle bugs.** → Reuse #534 graceful drain
  + `bootout`/`bootstrap`; the restart lifecycle test is the gate.
- **R: pattern set wrong, forcing rework in P4b–P4d.** → Land P4a fully (incl.
  redaction + restart) before starting the followers; treat it as the reference.

### BLAST RADIUS
`crates/trusty-search/` (new `ContractTool` impl + `restart` + version bump);
`trusty-common` (consumed). The controller is **not** modified here.

---

## P3 — DOC-5 dispatch engine + real passthrough + rewire `up/`

**Goal (one line):** build the 9-step universal dispatch loop, make
`src/commands/passthrough.rs` real, thread scope (kill TODO #920), then re-route
the `up/` orchestrator through it — **validated against trusty-search (P4a)**.

**Satisfies:** DOC-5 §2 (9-step dispatch), §3 (scope/blast-gate, M5), DOC-3 scope
threading + system advisory lock (M7) on the ensure rung; DOC-12 rewiring.

### ENTER GATE
- P2 EXIT GATE met (rollup is the real step-8 backend).
- P1 identity walk available (scope resolution needs `detect_project`; the slug
  key remains inert).
- **DOC-12 promoted to Accepted, OR the owner re-confirms Q1–Q6 in the decisions
  log (M5, D-M5)** — required before the `up/` rewiring work item begins.

### WORK ITEMS (each ≈ one PR)
1. **Implement the 9-step pipeline** (DOC-5 §2.1): load manifest → select members →
   resolve scope → blast-radius gate → capability-negotiate (`version --json` →
   `verbs[]` + `contract_version`; below-floor → `contract_incompatible`; verb not
   advertised → `na`) → invoke (parallel, global `--timeout`) → collect
   `Envelope<T>` → rollup/render (P2) → exit code.
2. **Make `src/commands/passthrough.rs` real** — `tctl <tool> <verb> [args]`
   forwards any **advertised** verb and renders the envelope verbatim; first-class
   commands become sugar over it. Reject `tctl <controller-id> <verb>`
   (self-reference, M5) with exit 3.
3. **Thread scope** — replace `src/main.rs`'s `let _ = cli.scope` (TODO #920) with
   the DOC-3 §3 default rule + per-verb scope polymorphism; wire `src/scope.rs`
   (kept in P0) into the resolver. Lifecycle verbs
   (`install/upgrade/start/stop/restart`) are **system only** —
   `--scope project` → usage error exit 3.
4. **Blast-radius gate (M5)** — system-mutating op + TTY + no `--yes` → confirm;
   **non-TTY system-mutating without `--yes` → exit 3** (fail loud).
5. **System advisory lock (M7) on the ensure rung** — currently only in
   `src/commands/up/`; extend it to `ensure`.
6. **Re-route `up/` through the Dispatcher/passthrough** — replace the raw
   `Command::new` calls in `src/commands/up/system_runner.rs` /
   `src/commands/up/os_env.rs` with dispatch through the contract path (behind the
   existing `Runner`/`ClaudeEnv`/`ConsoleLocator` traits, so this is mechanical).
   `up/` must now trust **envelope status**, not blind exit codes. (Gated by D-M5.)

### EXIT GATE (binary pass/fail)
- [ ] **Validated against a real member (B3):** dispatch + rollup are exercised
      end-to-end against **trusty-search (P4a merged)** emitting a **real
      `Envelope<T>` — not a fixture**.
- [ ] **A1 pulled forward:** `--json` `exit_code` field == process exit (asserted
      against the real search envelope).
- [ ] **A2 pulled forward:** the rollup matrix shape is well-formed
      (`verdict ∈ {ready,degraded,pending,down}`, `summary` counts sum incl. `na`,
      `cells.{system,project}`, `clusters[]`/`remediations[]` well-formed) over the
      real stack.
- [ ] **A8 pulled forward:** `tctl doctor --self-check trusty-search` passes
      (envelope shape + vocab + redaction).
- [ ] `tctl <tool> <verb>` passthrough forwards an advertised verb and renders the
      envelope; an **unadvertised** verb → `na`/usage error (exit 3); a self-target
      `tctl trusty-controller <verb>` → **rejected, exit 3** (M5).
- [ ] `src/main.rs` has **no** `let _ = cli.scope`; scope is resolved and threaded
      (TODO #920 closed). Test: `--scope project` on `install` → usage error exit 3.
- [ ] Non-TTY + system-mutating + no `--yes` → **exit 3** (test drives a piped
      stdin); TTY path prompts; `--yes` skips the prompt.
- [ ] Every first-class command routes through the **same** dispatch primitive
      (DOC-5 §2.2 proof): grep shows no per-named-tool branching in the dispatch
      path.
- [ ] `up/` STAGE 0–5 runs through the Dispatcher (no raw `Command::new` to member
      binaries in `src/commands/up/system_runner.rs` / `src/commands/up/os_env.rs`);
      `up/`'s member set == manifest set; `up/` trusts envelope status (regression
      test: a wedged daemon answering `degraded` is not treated as success).
- [ ] `ensure` acquires the M7 system advisory lock.
- [ ] `cargo test -p trusty-controller` green; clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated.

### RISKS + MITIGATIONS
- **R: `up/` is the most-built part and DOC-12 is DRAFT (caveats (b),(f),M5).** →
  ENTER GATE blocks the rewiring until DOC-12 is Accepted/Q1–Q6 re-confirmed
  (D-M5); rewire behind the existing traits (mechanical); keep STAGE 0–5 behaviour
  identical; gate with a before/after `up`-dry-run comparison.
- **R: dispatch validated only on fixtures (B3).** → The exit gate requires P4a
  (real trusty-search envelope) + A1/A2/A8 — no fixture-only acceptance.
- **R: scope threading regresses ensure/up behaviour.** → The harness skeleton +
  the explicit `--scope` usage-error tests catch this; pure-function unit tests
  will not (caveat (b)).

### BLAST RADIUS
`crates/trusty-controller/`: `src/commands/passthrough.rs` (real), new
`src/dispatch/` module, `src/scope.rs` (wired), `src/main.rs` (scope + flags),
`src/commands/*` (sugar-over-dispatch), `src/commands/up/system_runner.rs`,
`src/commands/up/os_env.rs`, `src/commands/up/` stages, `src/commands/ensure/`
(M7 lock).

---

## P4b/P4c/P4d — Remaining cross-crate retrofits (DOC-6 §2)

**Goal (one line):** retrofit the remaining members onto the DOC-1 envelope, in
dependency order, following the P4a pattern.

**Satisfies:** DOC-6 §2 conformance retrofits; closes caveat (a).

> **Each crate is an independently-gated sub-phase** (M4). P4d (review) is the
> long pole — it implements **none** of the 7 verbs today and is budgeted
> separately.

### ENTER GATE
- P4a landed (pattern set) and P3 landed (the controller can exercise each
  retrofit as it lands).

### WORK ITEMS (per crate ≈ one+ PR)
- **P4b — trusty-memory (T3)** — `ContractTool`, 7 verbs, `verbs[]`,
  `redact_value`, net-new `restart`; follow the search pattern.
- **P4c — trusty-analyze (T3)** — same retrofit, follow the search pattern.
- **P4d — trusty-review (T2, heaviest)** — implements **none** of the 7 verbs
  today; full retrofit. Expect the most work here; budget separately.
- **Per crate:** bump the crate version; add a `<binary> version --json` snapshot
  test; add a **per-crate restart-lifecycle test** (graceful drain #534); ensure
  `tctl doctor --self-check <member>` passes (DOC-6 §8); redaction negative
  assertion.

### EXIT GATE (binary pass/fail, per crate)
- [ ] `<binary> version --json` advertises `verbs[]` (the 7, or a documented subset
      for CLI-only members) and a `contract_version >= floor`.
- [ ] `tctl stack doctor --json` yields a typed envelope for the member (no
      synthesized `contract_incompatible` for a member that *should* be conformant).
- [ ] `tctl doctor --self-check <member>` passes (envelope shape + vocab +
      redaction); restart-lifecycle test green.
- [ ] Secrets redacted with `***redacted***` (negative conformance assertion).
- [ ] **Publish-order check (M3):** `trusty-common` published before the member if
      bumped; cdhash re-verified + FDA re-granted after each daemon `cargo install`.
- [ ] `cargo check` whole-workspace green; the crate's tests green;
      clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated **per crate** (each
      is its own gate row: P4b/P4c/P4d).

### RISKS + MITIGATIONS
- **R: scope creep across crates.** → Strictly dependency-ordered; P4a set the
  pattern so memory/analyze are near-mechanical; review is the known long pole —
  budgeted separately as P4d.
- **R: `restart` net-new on every daemon.** → Reuse #534 graceful drain +
  launchd convention; per-crate restart test under the harness.
- **R: version skew between controller and member.** → Golden snapshot (P1) +
  `min_contract_version` floor (P1 manifest) guard; bump+propagate pins together.

### BLAST RADIUS
`crates/trusty-memory/`, `crates/trusty-analyze/`, `crates/trusty-review/` (new
contract impls + `restart` + version bumps); `trusty-common` (consumed). The
controller is **not** modified here.

---

## P5 — claude-mpm `uv` shim (DOC-6 §4) — minimal, last

**Goal (one line):** give the external Python orchestrator a contract adapter behind
a `uv` shim so it advertises the same envelope/verbs — built **last and minimal**.

**Satisfies:** DOC-6 §4 / Resolved Decision 3/5 (orchestrator shim, `uv tool
install claude-mpm`), implementation-spine step 4.

> **Build minimal, T1 only.** Advertise exactly `["doctor","health","version"]`.
> This shim is **doomed when trusty-mpm lands** (it becomes the native Rust
> orchestrator member) — invest the minimum.

### ENTER GATE
- P4b/P4c/P4d substantially landed (the Rust members are conformant; the shim is
  the last member to make conformant).
- P3 dispatch routes `kind = "orchestrator"` through the shim path.

### WORK ITEMS (each ≈ one PR)
1. **`trusty_common::contract::orchestrator` shim** — synthesize the DOC-1 envelope
   for `doctor`/`health`/`version` from `mpm-doctor` + liveness probes; advertise
   exactly `["doctor","health","version"]`.
2. **Manifest member** — add claude-mpm as `kind = "orchestrator"`,
   `install = { source = "python", tool = "uv", package = "claude-mpm" }` in
   `manifest.toml`; pin the concrete version (the harness, P7a, resolves the pin
   per DOC-10 §6).
3. **Wire dispatch** — the controller routes the orchestrator member through the
   shim (keyed off `kind`, never the name).

### EXIT GATE (binary pass/fail)
- [ ] `tctl <orchestrator> version --json` (via shim) advertises exactly
      `["doctor","health","version"]`.
- [ ] `tctl stack doctor --json` includes the orchestrator as a normal envelope
      source (synthesized), not a hard failure when `uv`/claude-mpm absent → it
      degrades gracefully with the DOC-8 §5 guide-and-abort hint for `uv`.
- [ ] `tctl doctor --self-check <orchestrator>` passes (shim conformance).
- [ ] **Publish-order check (M3):** `trusty-common` published before consumers if
      the shim required a `trusty-common` bump.
- [ ] `cargo check`/tests/clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated.

### RISKS + MITIGATIONS
- **R: over-investing in a doomed shim.** → T1 only (3 verbs); no lifecycle verbs.
- **R: Python/uv environment fragility.** → The shim degrades to a
  guide-and-abort (exit 3 + `https://astral.sh/uv` hint) when `uv` is absent,
  asserted by harness A0.

### BLAST RADIUS
`crates/trusty-common/` (`src/contract/orchestrator` shim);
`crates/trusty-controller/manifest.toml` (orchestrator member);
`crates/trusty-controller/` dispatch (orchestrator routing).

---

## P6 — M15 re-key migration + slug-flag flip (DOC-6 §7.1) — HIGHEST RISK, ATOMIC

**Goal (one line):** migrate per-project state to the canonical full-path-slug id by
**re-keying** (renaming), and **flip the inert slug flag in the same phase** so the
flip and the migration are atomic (B1).

**Satisfies:** DOC-6 M15 (stateful re-key migration), DOC-6 §7.1 (migration
framework), ADR-0008 identity.

> **This is the single highest-risk item — isolate it in its own PR(s).** The live
> key today is the **basename** (`derive_index_id`); the canonical slug
> (`id_from_path`) is a *different* key. P1 landed the slug path **inert behind a
> flag**. **P6 re-keys all state by `root_path` AND flips the flag, atomically.**
> Done naively the flip orphans every index/palace, and a naive migration triggers
> a from-scratch reindex (multi-minute + storage doubling). The migration MUST
> **re-key by `root_path`** (rename the keyed state), **no re-embed / no reindex**.

> **Where the migration lives (B1-b):** in **trusty-search's existing
> `core::migration` `_meta` framework** (`m001..m004.rs`, idempotent/crash-safe),
> as a new `m005`-style step with a **`schema_version` bump** per DOC-6 §7.1 —
> **NOT** a controller/common helper.

### ENTER GATE
- P1 EXIT GATE met (the canonical `id_from_path` is hoisted and golden-tested, and
  is the migration's target key — but still **inert**).
- P4a–P4d landed (members consume the shared identity primitives; their on-disk
  state is what gets re-keyed).
- A **backup/rollback plan** recorded in the decisions log (this phase mutates
  user state on disk).

### WORK ITEMS (each ≈ one PR)
1. **Add the migration to trusty-search `core::migration`** — a new `_meta` step
   (m005-style) gated by a `schema_version` bump (DOC-6 §7.1); idempotent +
   crash-safe like the existing m001..m004.
2. **Inventory the keyed state** — enumerate every per-project store keyed by the
   old **basename** id (indexes, palaces, `.mcp.json` entries, controller state).
3. **Re-key by `root_path`** — for each store, compute the new canonical slug id
   from the stored `root_path` and **rename** the key/entry in place. **No reindex,
   no re-embed.** Idempotent: a second run is a no-op.
4. **`--dry-run` mode** — prints the planned re-keys without mutating.
5. **Flip the inert slug flag (ATOMIC, B1)** — only **after** the re-key step
   verifies, flip the flag so `id_from_path` (slug) becomes the live key. The flip
   and the re-key are the same migration unit; the system never has a live slug key
   without re-keyed state.
6. **Verification pass** — after migration, assert no store still uses an old
   basename id and the data byte-count is unchanged (no storage doubling).

### EXIT GATE (binary pass/fail)
- [ ] The migration lives in **trusty-search `core::migration`** with a
      `schema_version` bump (DOC-6 §7.1) — not a controller/common helper.
- [ ] Migration **re-keys** (renames) and does **not** reindex: a test asserts the
      index/palace byte-count is **unchanged** pre/post (no duplication).
- [ ] The slug flag flips **only after** the re-key verifies; a test proves that
      pre-flip the live key is the basename and post-migration the live key is the
      slug, with **zero** stores referencing an old basename id.
- [ ] `--dry-run` mutates nothing; a real run is **idempotent** (second run no-op).
- [ ] A rollback path is documented and tested (restore from the recorded backup;
      schema_version downgrade behaviour defined).
- [ ] **Publish-order check (M3):** `trusty-search`/`trusty-common` published in
      order; cdhash re-verified + FDA re-granted after install.
- [ ] `cargo test` green for the migration + every consumer crate's state tests;
      clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated; decisions log notes
      the migration window + rollback.

### RISKS + MITIGATIONS
- **R (B1): flip orphans all state.** → The flag stays inert through P1–P5; the
  flip is atomic with the re-key here; the "pre=basename / post=slug, zero old-id
  stores" test is the gate.
- **R: data loss / storage doubling.** → Re-key (rename) only; byte-count-unchanged
  assertion; `--dry-run` first; recorded backup.
- **R: partial migration leaves split state.** → Idempotent crash-safe `_meta`
  step + the "zero old-id stores remain" verification pass.

### BLAST RADIUS
`crates/trusty-search/src/core/migration/` (new `_meta` step + `schema_version`);
every stateful crate's on-disk store (`trusty-search` indexes, `trusty-memory`
palaces, controller state, project `.mcp.json`); the inert slug flag in
`trusty-common` (flipped here).

---

## P7a — DOC-10 isolation harness: CODE + A0–A8 battery

**Goal (one line):** implement the harness code and the full A0–A8 assertion
battery (code only — CI infra is P7b).

**Satisfies:** DOC-10 (MUC1/MUC2), §5 A0–A8 (A1/A2/A8 already proven in P3 against
the real search member; here they generalize to the full stack).

> The **minimal skeleton already exists from P0**, and **A1/A2/A8 were pulled
> forward to P3** (B3). P7a fills in the remaining battery (A0, A3–A7) and the
> macOS cdhash assertion logic. (Caveat (b): the harness is *why* the skeleton was
> pulled early — the 214 unit tests cannot catch dispatch/scope regressions.)

### ENTER GATE
- P3 EXIT GATE met (dispatch + scope + `up/` rewiring complete; A1/A2/A8 proven).
- P4a–P4d/P5 landed (the harness asserts real members + the orchestrator shim, A8).
- P6 landed (the harness runs against re-keyed/slug state, not legacy basename ids).

### WORK ITEMS (each ≈ one PR)
1. **Drive the canonical scenario** (DOC-10 §2 STEP 0–6): bootstrap toolchain →
   `cargo install trusty-controller` → `tctl install --yes` → `tctl ensure
   --scope project --wait --yes` → idempotency re-run → `tctl upgrade --yes` →
   verify-after.
2. **Implement the A0–A8 assertion battery:**
   - **A0** hard-dep guide-and-abort → exit 3 + `https://sh.rustup.rs` /
     `https://astral.sh/uv` stderr hints.
   - **A1** exit-code mirror (`--json` `exit_code` == process exit). *(generalized
     from P3.)*
   - **A2** rollup matrix shape. *(generalized from P3.)*
   - **A3** version truth post-install **and** post-upgrade (`stack_version` +
     each `tool_version` == BOM pin), asserted **twice**.
   - **A4** daemons actually running (`health --json` status == `running`).
   - **A5** `.mcp.json` patched (each enabled two-layer member entry present).
   - **A6** idempotency (second install/ensure → no-op).
   - **A7** progressive readiness non-racy (`ensure --wait` → `fresh`).
   - **A8** conformance self-check per member (+ mpm shim 3-verb). *(generalized
     from P3.)*
3. **macOS cdhash/warm-boot assertion logic (M14, §3.4)** — assert the on-PATH
   binary execs without SIGKILL via `cargo install`'s atomic-rename (the forbidden
   `cp`-over path is **not** used). (Wiring to a runner is P7b.)

### EXIT GATE (binary pass/fail)
- [ ] `tests/isolation_harness.rs` runs the full A0–A8 battery **locally** (green).
- [ ] The canonical STEP 0–6 scenario runs **non-interactively** (`--yes`, non-TTY)
      and asserts only machine fields (no scraped human text).
- [ ] A deliberately-broken member (local throwaway) makes the harness **fail** with
      the failing cell surfaced from `clusters[]`/`remediations[]` (proves the
      oracle is real), then reverted.
- [ ] `cargo test`/clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated.

### RISKS + MITIGATIONS
- **R: flaky background-reindex races.** → `ensure --wait` (A7) gates on `fresh`.
- **R: cdhash SIGKILL trap (§3.4, CLAUDE.md #873).** → The harness only ever
  installs via `cargo install`/`tctl`, never `cp`; the cdhash assertion fails if
  that property regresses.

### BLAST RADIUS
`crates/trusty-controller/tests/isolation_harness.rs`, harness fixtures (sample
repo, BOM N-1 seed). No production-code changes (capstone validation only).

---

## P7b — DOC-10 isolation CI infra: macOS-VM / Linux-container + `isolation.yml`

**Goal (one line):** stand up the CI isolation environment and wire `isolation.yml`
so the P7a battery runs in a vanilla environment on macOS primary / Linux
secondary.

**Satisfies:** DOC-10 §3/§4 (vanilla-env construction), §3.4 cdhash, M14 warm-boot.

> Split out from the harness code (M4) because the **environment/provisioning** is
> a distinct, higher-risk concern. **GitHub-hosted macOS runners cannot trivially
> run nested VMs** — provisioning a nested VM is a known risk; per DOC-10 §3 the
> **`macos-14` runner itself is the isolation host** (a clean `$HOME` + clean PATH
> on the runner, not a nested VM, is the realistic v1 isolation; launchd is
> per-user, so a separate user session / runner is the isolation boundary).

### ENTER GATE
- P7a EXIT GATE met (the A0–A8 battery runs green locally).

### WORK ITEMS (each ≈ one PR)
1. **Vanilla-env construction** (DOC-10 §3/§4) — clean `$HOME`, no cargo/uv/tool
   binaries on PATH; **macOS:** the `macos-14` runner as isolation host (clean
   `$HOME`/PATH; **document the nested-VM limitation** and why the runner-as-host
   approach is used); **Linux:** a container.
2. **CI wiring (`isolation.yml`)** — promote the P0 stub to the full job: nightly
   full §2 scenario + per-PR path-filtered macOS smoke + Linux legs.
3. **macOS cdhash/warm-boot CI leg (M14)** — a per-PR `macos-14` smoke job:
   `cargo install` → daemon up under launchd → `health` == running → `restart`
   (`bootout`→`bootstrap`) → `health` again; **no `cp`-over-PATH**.

### EXIT GATE (binary pass/fail)
- [ ] The full **A0–A8 battery is green on `macos-14`** (and on the Linux leg).
- [ ] macOS cdhash assertion green in CI (install via `cargo install` → daemon up
      under launchd → `health` running → `restart` → `health`; **no `cp`-over-PATH**).
- [ ] `isolation.yml` runs the macOS smoke **per PR** (path-filtered) and the full
      scenario nightly; the Linux leg runs in a container.
- [ ] The nested-VM limitation + runner-as-isolation-host approach is documented in
      `isolation.yml`/harness docs.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated.

### RISKS + MITIGATIONS
- **R: nested VMs not feasible on GitHub-hosted macOS runners (M4).** → Use the
  `macos-14` runner itself as the isolation host (clean `$HOME`/PATH); document the
  trade-off; revisit self-hosted runners only if true VM isolation is mandated.
- **R: macOS launchd-per-user contamination (§3).** → Use a separate user
  session / runner as the boundary; do not rely on a clean `$HOME` alone.

### BLAST RADIUS
`.github/workflows/isolation.yml`, CI runner config, harness fixtures. No
production-code changes.

---

## P8 — Fast-follow: transitive cluster fold (C4) + DOC-11 residuals

**Goal (one line):** add the transitive `depends_on ∪ deps[]` cluster fold and clear
the residual DOC-11 follow-ups (re-routing stale DOC-7 → trusty-console).

**Satisfies:** DOC-4 C4 (deferred from P2), DOC-11 residual tracker items.

### ENTER GATE
- P2 EXIT GATE met (the lattice + matrix exist; `clusters[]` is a valid-but-empty
  array ready to be populated).
- P7a/P7b green (so the C4 change can be regression-tested end to end).

### WORK ITEMS (each ≈ one PR)
1. **Transitive cluster fold (C4)** — fold member verdicts over the union of
   manifest `depends_on` and envelope `health.data.deps[]`; populate `clusters[]`
   in the `--json` rollup so a failing dependency surfaces its dependents.
2. **DOC-11 residual sweep** — re-route the follow-ups that still point at the
   superseded DOC-7 embedded UI to `trusty-console` (caveat (e)); close or
   re-file remaining DOC-11 items.
3. **Re-audit the README + abbreviation table** — confirm `tctl →
   trusty-controller` is present (DOC-0 follow-up) and the README reflects the
   now-complete surface.

### EXIT GATE (binary pass/fail)
- [ ] `tctl stack doctor --json` populates `clusters[]`: a test where
      `trusty-analyze depends_on trusty-search` and search is `down` surfaces
      analyze in the same cluster with the correct rolled verdict.
- [ ] No DOC-11 follow-up still references the superseded DOC-7 embedded UI as a
      live target (all re-routed to trusty-console or closed).
- [ ] README + abbreviation table accurate; `tctl` alias present.
- [ ] Full A0–A8 harness still green; `cargo test`/clippy/fmt/line-cap clean.
- [ ] [`progress-manifest.md`](./progress-manifest.md) updated; campaign marked
      complete.

### RISKS + MITIGATIONS
- **R: C4 is speculative at 7 members and may over-engineer.** → Deliberately
  deferred from P2; keep the fold simple (single transitive pass) and gate it
  behind the harness.
- **R: DOC-11 has stale UI items that mislead future readers (caveat (e)).** →
  Re-route every DOC-7 reference to trusty-console; record in the decisions log.

### BLAST RADIUS
`crates/trusty-controller/`: `src/rollup/` (cluster fold), `src/commands/output.rs`
(`clusters[]`), `README.md`;
`docs/trusty-controller/research/02-design/DOC-11-open-issues.md` (tracker
updates, documentation-only).

---

## Carried-but-unscheduled / backlog (M4)

Items that are real but not yet placed in a numbered phase. Each carries a
tentative **owner-phase**; promote into a phase (or a follow-up campaign) when
scheduled. Mirrored in the progress manifest.

| # | Item | Source | Tentative owner-phase |
|---|---|---|---|
| BL-1 | DOC-8 §4 launch-hook (Claude Code SessionStart) **verification** | DOC-8 §4 | P3 or P7a (harness assertion) |
| BL-2 | `--latest` / BOM-drift consumer | `updates.rs:67` TODO #1316 | post-P1 (needs manifest+floor) |
| BL-3 | Upgrade **rollback / pin-back** (`tctl upgrade --to`) testing | DOC-9 | P7a (harness) |
| BL-4 | Controller-side redaction **belt-and-suspenders** + CI **negative-assertion** that it fires when a member forgets to redact | DOC-1 D8 | P1 (helper) + P4a–d (assertion) |
| BL-5 | **Arg-surface guard** — golden snapshot does NOT cover CLI arg-surface breaking changes | DOC-1 C3 gap | P1 (add guard) |
| BL-6 | **Observability/tracing** for the dispatch path | DOC-5 | P3 (instrument) |

---

## Cross-phase invariants

> See the **CROSS-PHASE INVARIANTS** section near the top of this document (it is
> referenced from every phase that bumps `trusty-common` or installs a daemon
> binary). The short list: doc/error rules; 500/1500 SLOC caps; shared-crate
> publish-order + dry-run + whole-workspace check; macOS FDA/cdhash after every
> daemon install; worktree discipline; manifest-is-the-only-registry after P0;
> update the (advisory) progress manifest at every gate.
