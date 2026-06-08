# trusty-controller — Design Document Set

**Status:** Draft — stub set (DOC-0 charter is **Accepted**)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

> **Location:** this design set lives under `docs/trusty-controller/research/`
> following the repo's per-crate documentation convention (`01-spec/` is the
> frozen source-of-record; `02-design/` is this refinement layer). It was
> relocated here from the former `docs-local/02-trusty-controller/` path.
>
> **DOC-0 is Accepted.** The naming & documentation charter is resolved: the
> crate is **`trusty-controller`** (dir `crates/trusty-controller/`), the binary
> is **`tctl`**, and the alias is **`tctl`**. The naming decision is recorded as
> an ADR ([`docs/adr/0006-trusty-controller-naming.md`](../../../adr/0006-trusty-controller-naming.md)).
> See [`00-naming-and-doc-charter.md`](./00-naming-and-doc-charter.md) for the
> full set of decisions (scope framing, publishing, documentation charter).

## Rationale

The spec proposes **trusty-controller**: a single, thin coordinator that manages
install, upgrade, restart, configuration, doctor, and health across the whole
claude-mpm stack (claude-mpm + trusty-tools) at both the **system** and
**project** scope. The controller must contain *zero tool-specific logic* — it
operates entirely through a **versioned per-tool contract** and a **stack
manifest/BOM** that doubles as its tool registry.

This design set decomposes that vision into 11 focused design docs. Each refines
one concern of the spec into an implementable design. The set is deliberately
sequenced so the two foundational contracts (the tool contract, DOC-1; and the
stack manifest, DOC-2) land first, because nearly everything else consumes them.

The docs are working stubs: Open Questions, Dependencies, and Grounding are
populated with real content; full schemas, prose, and decisions are TODO.

## Dependency graph

```
                         DOC-0 (naming & charter)
                                  │  name <name> flows to ALL docs
                                  ▼
              ┌──────────── DOC-1 (tool contract) ◄────► DOC-3 (scope model)
              │                   │                          │
              │                   ▼                          ▼
              │            DOC-2 (manifest/BOM)        (scope schema feeds DOC-1)
              │             │   │   │   │   │
              ▼             ▼   ▼   ▼   ▼   ▼
        DOC-6 (conformance + mpm adapter)
              │  gates ─────────────────────────► DOC-10
              ▼
        DOC-4 (doctor/health rollup)
              │
              ▼
        DOC-5 (controller CLI + dispatch)
         │    │    │
         ▼    ▼    ▼
   DOC-7   DOC-8   DOC-9
  (web UI)(install)(upgrade)
              │      │
              ▼      ▼
            DOC-10 (isolation testing harness)
```

Edges (consumes → produced-by) in detail:

- **DOC-0** produces the chosen `<name>` consumed by every other doc.
- **DOC-1 ◄──► DOC-3** are bidirectional: DOC-1 owns the wire format of `scope`;
  DOC-3 owns its behavioral model and feeds schema fields back to DOC-1.
- **DOC-1** produces inputs for DOC-2, DOC-4, DOC-5, DOC-6, DOC-7, DOC-9.
- **DOC-2** produces inputs for DOC-5, DOC-6, DOC-7, DOC-8, DOC-9.
- **DOC-3** produces inputs for DOC-1, DOC-4, DOC-5, DOC-8.
- **DOC-4** produces inputs for DOC-5, DOC-7, DOC-10.
- **DOC-5** produces inputs for DOC-7, DOC-8, DOC-9, DOC-10.
- **DOC-6** gates DOC-10 (only conformant tools can be rolled up/tested) and
  produces inputs for DOC-4, DOC-5.
- **DOC-7, DOC-10** are terminal (nothing downstream depends on them).
- **DOC-8, DOC-9** produce inputs for DOC-10.

## Document index

| Doc | Title | One-line purpose |
|---|---|---|
| **DOC-0** | Naming & Documentation Charter | Pick the tool's real name (binary/crate/dir) and lock doc conventions. |
| **DOC-1** | The Versioned Tool Contract | Define the exact versioned interface every stack member implements (FOUNDATIONAL). |
| **DOC-2** | Stack Manifest/BOM + Version & Changelog Advertisement | Define the manifest/BOM/lockfile + tool registry + structured changelogs (FOUNDATIONAL). |
| **DOC-3** | Scope Model (System vs Project) | Specify the behavioral model behind the scope axis (readiness, idempotency, blast radius). |
| **DOC-4** | Doctor/Health Rollup Model | Aggregate per-tool doctor/health JSON into a stack verdict + tools×scope matrix. |
| **DOC-5** | Controller CLI Command Surface + Dispatch | Define the controller's own CLI and manifest-driven verb dispatch. |
| **DOC-6** | Per-Tool Contract Conformance + claude-mpm Python Adapter | Audit each tool vs DOC-1; specify how Python claude-mpm satisfies the contract. |
| **DOC-7** | Controller Web UI (link-out control plane) | Out-of-the-box UI that links out to each tool's existing UI, never reimplementing. |
| **DOC-8** | Install/Bootstrap Flow (UUC1, UUC2) | Zero-knowledge install + per-project auto-config on claude-mpm launch. |
| **DOC-9** | Upgrade Flow (UUC3) | Cross-tool update detection, changelog headlines, upgrade + take-effect restart. |
| **DOC-10** | Isolation Testing Harness (MUC1, MUC2) | Test stack install/upgrade in a vanilla container/VM without contaminating the host. |

## Wave-based sequencing

- **Wave 0 — Charter:** DOC-0 (naming). Blocks every filename, manifest entry,
  and contract example downstream.
- **Wave 1 — Foundations:** DOC-1 (tool contract) and DOC-3 (scope model),
  developed together due to the bidirectional `scope` edge, then DOC-2
  (manifest/BOM). These three are the load-bearing contracts.
- **Wave 2 — Conformance & rollup:** DOC-6 (per-tool conformance + mpm adapter)
  and DOC-4 (doctor/health rollup). DOC-6 gates downstream testing; DOC-4 turns
  per-tool JSON into a stack verdict.
- **Wave 3 — Surfaces:** DOC-5 (controller CLI + dispatch), then the user-facing
  surfaces DOC-7 (web UI), DOC-8 (install/bootstrap), DOC-9 (upgrade).
- **Wave 4 — Validation:** DOC-10 (isolation testing harness), which exercises
  the whole stack end-to-end.

## Cross-cutting concerns

These themes recur across multiple docs and must stay consistent:

- **Contract-versioning behavior** — how the controller negotiates and degrades
  gracefully against tools on an older `contract_version` (render degraded, not
  fail). Flagged in DOC-1, DOC-4, DOC-5, DOC-7, DOC-9.
- **Isolation-testability** — every install/upgrade path must be runnable
  non-interactively and side-effect-scoped so it can be exercised in a VM/container.
  Flagged in DOC-8, DOC-9, DOC-10.
- **Security / secrets** — no secrets in contract output or in the manifest;
  daemons are loopback-only with no auth and the UI must match. Flagged in DOC-1,
  DOC-2, DOC-7.
- **Project-identity convention** — a shared rule (git root → index-id/palace-id)
  binds project-scope ops to the right cwd. Defined in DOC-3, referenced by
  DOC-6 and DOC-8.
- **Orchestrator-swap forward-compatibility** (DOC-0 charter, A4) — the
  orchestrator is a pluggable stack member: **claude-mpm** (Python, external) is
  the current stable orchestrator, coordinated via a Python contract adapter
  (DOC-6); **trusty-mpm** (`crates/trusty-mpm`, Rust) is the planned in-house
  replacement, not yet ready. The contract + manifest (DOC-1/DOC-2) and the
  conformance/adapter doc (DOC-6) must treat the orchestrator as swappable
  (claude-mpm now → trusty-mpm later) without controller changes.

## TODO

- [x] Resolve DOC-0 naming (`trusty-controller` / `tctl`) — recorded in
      `docs/adr/0006-trusty-controller-naming.md`; downstream filenames/examples
      can now be finalized
- [ ] Promote DOC-1 and DOC-2 from stub to full schema definitions
- [ ] Review the dependency graph with the team
