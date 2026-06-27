# 00 — Current State Snapshot (trusty-controller / `tctl`)

> **Self-contained.** This file restates everything a plan reader needs about the
> *as-built* state of `crates/trusty-controller/` so they can read
> [`01-phased-plan.md`](./01-phased-plan.md) with no prior context. It is the
> authoritative reconciliation between the **02-design** doc set (the intended
> design) and the code that actually shipped.
>
> **Source of truth for intent:** [`../02-design/`](../02-design/) (DOC-0..DOC-12).
> **Source of truth for "what's built":** this snapshot, derived from a verified
> reconciliation pass over `crates/trusty-controller/` at `v0.2.0`.

---

## 1. Crate facts

| Fact | Value |
|---|---|
| Crate / dir | `trusty-controller` / `crates/trusty-controller/` |
| Binary + alias | `tctl` |
| Version | `0.2.0` |
| Publishable | Yes (publish is enabled) |
| Tests | **214 inline unit tests**; **NO `tests/` integration directory** |
| License | `Elastic-2.0` via `license-file = "LICENSE"` (DOC-0 A5) |
| README | **Badly stale** — claims "Phase 0 scaffold" and `publish = false` although everything in §2 below is shipped. Must be rewritten (P0). |

**Critical testing gap:** all 214 tests are **pure-function unit tests**. They do
**not** exercise dispatch, scope threading, or end-to-end install/upgrade. They
will **not** catch dispatch/scope regressions. This is why a harness skeleton is
pulled *early* (P0), harness assertions A1/A2/A8 gate **P3** against a real member,
and the full battery / CI infra land in **P7a/P7b** (see
[`01-phased-plan.md`](./01-phased-plan.md)).

---

## 2. DONE & real (shipped, working)

These commands/flows are implemented and functioning:

- `install` — install members.
- `upgrade` / `updates` — upgrade + list-updates.
- `ensure` (incl. `--wait`): `.mcp.json` patch, index register, palace create,
  readiness poll.
- `start` / `stop` / `restart` — launchd lifecycle.
- `config` — config rendering.
- `port` — report controller port.
- `doctor --self-check` — **ad-hoc** member self-check (not yet contract-driven).
- `ui` — launches `trusty-console` (per ADR-0011, superseding DOC-7's embedded UI).
- `version` — present but emits **placeholder data** (see §3).
- The full **`up/` bootstrap orchestrator** (DOC-12): STAGE 0–5, a **real `flock`
  lock**, prompt-first `claude_code` stage. This is the **most-built** part of the
  crate — but it is built on the **stubbed foundations** of §3, so it will need
  **rewiring** through the real Dispatcher/passthrough/manifest once those land.

---

## 3. SUBSET STAND-INS (will be REWRITTEN, not extended)

These exist but implement a degenerate subset of the design. The plan **replaces**
them rather than growing them:

> **Path note (m1):** these files live under `src/commands/...` (e.g.
> `src/commands/stack/health.rs`, `src/commands/probe.rs`); `scope.rs` is at
> `src/scope.rs`. Paths below are spelled out fully.

| File | What it does today | What the design (DOC) requires |
|---|---|---|
| `src/commands/stack/health.rs` | flat 2-value `.any()` fold | DOC-4 §2: 4-value lattice + tools×scope matrix + clusters |
| `src/commands/stack/doctor.rs` | flat 2-value `.any()` fold | DOC-4 §2/§3: deep matrix + drill-down + remediation |
| `src/commands/probe.rs` | ad-hoc `serde_json::Value` envelope validation | DOC-1 D3a: typed `Envelope<T>` parse |
| `src/commands/version.rs` / `src/commands/output.rs` | `stack_version = "0.0.0-scaffold"`, `contract_floor = 1` literal | DOC-2: real `stack_version = "YYYY.MM-N"` from manifest; DOC-1 D2: real floor |

---

## 4. DEAD / ORPHANED code (decide fate in P0)

- `src/scope.rs` — **built, zero non-test callers.** The scope primitive exists but
  is never wired. (`src/main.rs` has `let _ = cli.scope` with `TODO(#920)`.)
- `src/commands/auto_update.rs::maybe_notify` — **built, zero callers.** Speculative.
  Fate is now an **OPEN DECISION (D-m3)**: delete the orphan vs wire it to the DOC-9
  drift notification. Not pre-decided here (see
  [`02-adversarial-review.md`](./02-adversarial-review.md) m3 and the decisions log).

---

## 5. THE ONE TRUE STUB

- `src/commands/passthrough.rs` — `print_not_yet_implemented`. Yet the design (DOC-5 §2) makes
  this the **substrate of the entire 9-step dispatch engine**. Every first-class
  command is meant to be sugar over passthrough. **This is the single most
  load-bearing unbuilt piece in the crate.**

---

## 6. THE FOUR MISSING FOUNDATIONS

Nothing downstream can be correct until these land. (Build order: DOC-2 → DOC-1 →
DOC-4 → DOC-5; see [`01-phased-plan.md`](./01-phased-plan.md).)

### Foundation 1 — DOC-1 contract module (`trusty_common::contract`) **DOES NOT EXIST**

Required (DOC-1 D1–D8, ADR-0007):
- `Envelope<T>` uniform response (`contract_version`, `tool`, `tool_version`,
  `verb`, `scope`, `status`, `data`, `messages`).
- `contract_version`: **monotonic integer**, floor `F = 1` (D2). NOT semver.
- `verbs[]` capability advertisement — **independent of `contract_version`** (D3,
  the "versioning split"). The 7 verbs: `doctor health version start stop restart
  config`.
- `ContractTool` trait + `Dispatcher` (D6).
- **Golden-snapshot CI test (C3)** that fails any envelope/`data`-shape change
  unless `contract_version` is bumped + a ledger row added (D3).
- **Redaction marker `***redacted***` (D8)** — layered: tool-side `redact_value`
  primary line + controller-side belt-and-suspenders pass.

Current reality: **no member emits `contract_version` or advertises `verbs[]`.**
The `restart` verb is **net-new on every daemon**. `trusty-review` implements
**none** of the 7 verbs. → This foundation is only worth funding if the cross-crate
retrofit epic (Foundation/DOC-6, plan P4) is funded **in the same campaign**, else
the controller validates envelopes no member emits.

### Foundation 2 — DOC-2 manifest / BOM **NOT LOADED**

- `--manifest` flag is **inert**.
- **FOUR divergent hard-coded member lists** that already disagree on
  `review`/`tga`/`controller`:

  | Source | # members | Notable contents |
  |---|---|---|
  | `src/commands/up/manifest.rs::default_manifest()` | 6 | incl. `controller` |
  | `src/commands/stable_set.rs` | 7 | incl. `review` + `tga` |
  | `src/commands/up/manifest.rs::analyze_core_targets()` | 5 | — |
  | `src/commands/ensure/mcp_patch.rs::mcp_members()` | 4 | — |

  These four lists are the single largest correctness hazard. Collapsing them into
  one `manifest.toml` is the first work item of the whole campaign. Note
  `analyze_core_targets` is **also read** via `cfg.analyze_core_targets` in
  `src/commands/up/policy.rs` — that read is allowed to remain *as a manifest-backed
  query* after the collapse (see [`01-phased-plan.md`](./01-phased-plan.md) P0 EXIT
  GATE allowed-remaining-refs).

Design (DOC-2): **TOML**; **embedded default + system override only** (NO project
tier); `include_str!` the committed `manifest.toml`; `[[member]]` schema with
`id`, `display_name`, `binary`, `kind`, `install{source}`, `version` pin,
`min_contract_version`, `changelog`, `enabled`, per-member `timeout`,
`depends_on`; `stack_version = "YYYY.MM-N"`.

### Foundation 3 — DOC-3 scope threading **NOT WIRED**

- `src/main.rs`: `let _ = cli.scope` — `TODO(#920)`.
- `src/scope.rs` is dead (§4).
- Canonical project id must be the **full-path slug of the nearest git root**
  (canonicalize-then-slug, ADR-0008). The **slug primitive** is already hoisted
  (`trusty_common::slugify_string`), but the **detection walk**
  (`detect_project` / `id_from_path`) is **NOT** hoisted.
- The **system advisory lock (M7)** exists only in `src/commands/up/`, not on the
  `ensure` rung.

> 🔴 **CRITICAL (B1) — the identity flip is a LIVE re-key, not a "subtle id
> change."** The **live keying scheme today is the BASENAME**, not the slug:
> `trusty_common::derive_index_id` returns the basename (`index_id.rs:71` doc:
> *"changing casing/punctuation would orphan every previously-indexed project"*);
> `trusty-search/src/detect.rs` keys on the basename. The slug form
> (`fs_discovery.rs::id_from_path`) is a **different live key**. Flipping
> `derive_index_id` (basename) → `id_from_path` (slug) **re-keys every existing
> index and palace** and **orphans all user state** unless the re-key migration
> co-ships. Therefore the slug path must be **inert until the migration lands**
> (see §8 caveat (c) and [`01-phased-plan.md`](./01-phased-plan.md) P1 + P6).

### Foundation 4 — DOC-4 rollup **degenerate**

Needs (DOC-4 §2):
- **4-value lattice** `down ≻ degraded ≻ pending ≻ ready` (`skipped` absorbed).
- **tools×scope matrix**: the **system track (GLOBAL) drives the verdict**; a
  **project `pending` (LOCAL) never degrades the global verdict** — the
  load-bearing *"unindexed project ≠ broken stack"* rule that the **current flat
  fold gets WRONG**.
- **Transitive cluster fold** over `depends_on ∪ deps[]` (C4).
- **2 s / 10 s** timeouts (health / doctor). **Per DOC-4 Resolved Decision 1
  (owner-approved 2026-06-08), v1 ships ONLY a single global `--timeout` flag;
  per-member manifest timeout overrides are DEFERRED.** (The draft's "RD1 + M10"
  was wrong on both counts — RD1 *defers* per-member timeouts, and "M10" does not
  exist in DOC-4. See [`02-adversarial-review.md`](./02-adversarial-review.md) B2
  and OPEN DECISION D-B2.)
- Exit codes `0/2/1/3`; **stack aggregate = worst-wins `3 ≻ 1 ≻ 2 ≻ 0`**. Per
  **DOC-4 RD3**, a **project-scope** `down`/`fail` degrades **only the project
  cell** and does **NOT** raise the process exit code (the system track drives the
  exit). See P2 EXIT GATE (m6).
- `pending_since` time-escalation (a stalled `pending` past a budget → `degraded`).

---

## 7. The rest of the design surface (state at a glance)

| DOC | Concern | State |
|---|---|---|
| **DOC-5** | 9-step universal dispatch (load manifest → select → scope → blast-gate → capability-negotiate → invoke → collect → rollup → exit); passthrough is substrate; verb-aware; reject controller-self (M5); non-TTY system-mutating w/o `--yes` → exit 3 | dispatch engine unbuilt; `src/commands/passthrough.rs` is the stub (§5) |
| **DOC-6** | per-tool retrofit **across 4 OTHER crates** (search T3 pattern-setter → memory/analyze T3 → review T2, none built) + claude-mpm `uv` shim in `trusty_common::contract::orchestrator` (T1, `["doctor","health","version"]`) + **M15 stateful re-key migration carried in trusty-search `core::migration` (m001..m004 framework) with a `schema_version` bump (DOC-6 §7.1)** | unbuilt; multi-PR cross-crate iceberg |
| **DOC-8/9** | cargo install/verify **DONE**; BOM-pin / `--latest` drift **MISSING** (needs manifest); uv/python orchestrator install **MISSING**; launch-hook (Claude Code SessionStart) **needs verification** | partial |
| **DOC-10** | isolation harness (`tests/isolation_harness.rs` + `isolation.yml` + A0–A8 battery + macOS cdhash/warm-boot assertion M14) | **entirely unbuilt** |
| **DOC-12** (DRAFT) | `src/commands/up/` STAGE 0–5 **DONE** but on stubbed foundations; `src/commands/up/system_runner.rs`/`os_env.rs` use raw `Command::new`, **bypassing passthrough** and trusting status blindly → needs **rewiring** through Dispatcher/passthrough/manifest. **DOC-12 is still Draft** — P3 must not rewire `up/` until DOC-12 is Accepted (or Q1–Q6 re-confirmed): OPEN DECISION D-M5. | built-but-must-rewire |
| **DOC-7** | embedded web UI | **SUPERSEDED by ADR-0011**; targets moved to `trusty-console`. DOC-11 still routes some UI follow-ups here — those are stale. |

---

## 8. Devil's-advocate caveats (carry into every phase)

These are **settled findings** and must be honoured by the plan:

- **(a) DOC-1 only pays off with DOC-6 funded in the same campaign** — else the
  controller validates envelopes no member emits. **This makes P1 (contract) and
  P4 (retrofit) a single sub-campaign that must be justified together** — they are
  *not* the assumed spine. See the Option A / Option B strategic choice in
  [`01-phased-plan.md`](./01-phased-plan.md) and the DECISION GATE after P0/P2.
- **(b) Build the DOC-10 integration harness EARLY** — the 214 unit tests are
  pure-function and will not catch dispatch/scope regressions. **Corollary (B3):**
  P2/P3 acceptance must be validated against ≥1 **real** member envelope (the
  trusty-search retrofit, pulled forward to gate P3), not synthetic fixtures.
- **(c) The identity flip orphans ALL state unless the re-key migration co-ships.**
  The live key today is the **basename** (`derive_index_id`); the canonical slug
  (`id_from_path`) is a *different* live key. The flip must be **inert behind a
  flag until the migration lands**, and the migration must **re-key by `root_path`**
  (no re-embed/no reindex) inside **trusty-search `core::migration`** with a
  `schema_version` bump (DOC-6 §7.1). A naive flip-then-migrate-later orphans every
  index and palace; a naive migration triggers a from-scratch reindex (multi-minute
  + storage doubling).
- **(d) Timeouts: honor DOC-4 RD1 — v1 ships ONLY a global `--timeout`.** Per-member
  manifest overrides are **DEFERRED** (RD1, owner-approved 2026-06-08). The draft's
  "RD1 + M10" was wrong: RD1 defers per-member timeouts and "M10" does not exist.
  Un-deferring is an OPEN DECISION (D-B2) requiring explicit owner reversal of RD1.
- **(e) DOC-11 still routes UI follow-ups to the superseded DOC-7** — those targets
  moved to `trusty-console`; do not re-implement an embedded UI.
- **(f) DOC-12 is DRAFT while its `up/` is the most-built part** — treat DOC-12 as
  subject to change; expect rewiring, not preservation. **P3 may not rewire `up/`
  until DOC-12 is Accepted or Q1–Q6 re-confirmed (D-M5).**

---

## 9. Forced build order (post-adversarial-review)

Revised after the adversarial pass ([`02-adversarial-review.md`](./02-adversarial-review.md)).
The key structural changes vs the draft: (1) the identity-slug path is **inert
until its migration co-ships**; (2) the **trusty-search retrofit (P4a) is pulled
forward to gate P3** so dispatch is validated against a real member; (3) the
cross-crate epic is split P4a–P4d; (4) the harness is split P7a (code) / P7b
(CI infra); (5) a strategic **DECISION GATE (Option A vs B)** sits after P0/P2.

```
P0  DOC-2 manifest (FIRST — no upstream dep; kills 4-list divergence; unblocks
        stack_version + member selection + dispatch step 1)
        NON-contract fields only (M2 two-pass); + README rewrite; + real stack_version
   ↓
P2  DOC-4 lattice + scope matrix  (DEFER cluster fold C4 → P8)
        [Option-A deliverable ends here: README + 4-list collapse + real rollup,
         NO identity flip, NO cross-crate epic]
   ↓
======================  DECISION GATE — Option A vs Option B  ======================
   (stop at the value-first deliverable, OR commit P1+P4 together as a sub-campaign)
   ↓ (Option B only)
P1  DOC-1 contract module + identity-walk hoist (slug path INERT behind flag) +
        finalize manifest CONTRACT fields (M2 second pass)
   ↓
P3  DOC-5 dispatch loop + real passthrough + thread scope + rewire up/
        ── HARD-GATED by P4a (trusty-search emits a REAL Envelope<T>) ──
        ── A1/A2/A8 pulled forward to gate P3 ──
   ↓
P4a trusty-search retrofit (pattern-setter; lands inside/before P3 exit)
P4b trusty-memory  ·  P4c trusty-analyze  ·  P4d trusty-review (heaviest; none of 7 verbs)
   ↓
P5  claude-mpm uv shim (LAST, minimal, T1 only)
   ↓
P6  M15 re-key migration  +  FLIP the inert slug flag  (ATOMIC within one phase;
        carried in trusty-search core::migration, schema_version bump, DOC-6 §7.1;
        re-key by root_path, NO reindex)
   ↓
P7a DOC-10 harness CODE + A0–A8 battery   ·   P7b CI INFRA (macOS-VM/Linux-container)
   ↓
P8  transitive cluster fold C4 + DOC-11 residuals
```

Also throughout: rewrite stale README (P0); `auto_update` fate is OPEN DECISION
D-m3 (not pre-decided); `src/scope.rs` retained, wired in P3.
