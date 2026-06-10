# trusty-controller — Design Document Set

**Status:** Complete — all 11 docs Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

> **Location:** this design set lives under `docs/trusty-controller/research/`
> following the repo's per-crate documentation convention (`01-spec/` is the
> frozen source-of-record; `02-design/` is this refinement layer). It was
> relocated here from the former `docs-local/02-trusty-controller/` path.
>
> **The set is finished and handoff-ready.** All 11 docs (DOC-0 through DOC-10)
> are Accepted (owner-approved). Three decisions are escalated to ADRs:
> [ADR-0006](../../../adr/0006-trusty-controller-naming.md) (naming),
> [ADR-0007](../../../adr/0007-tool-contract-versioning-and-verb-model.md)
> (contract versioning + verb model), and
> [ADR-0008](../../../adr/0008-project-identity-convention.md) (project identity).
> The crate is **`trusty-controller`** (dir `crates/trusty-controller/`), the
> binary and alias are **`tctl`**. See
> [`00-naming-and-doc-charter.md`](./00-naming-and-doc-charter.md) for the full
> charter (scope framing, publishing, documentation conventions).

## Rationale

The spec proposes **trusty-controller**: a single, thin coordinator that manages
install, upgrade, restart, configuration, doctor, and health across the whole
claude-mpm stack (claude-mpm + trusty-tools) at both the **system** and
**project** scope. The controller's dispatch engine contains *zero per-tool
verb-dispatch logic* (no per-named-tool branching; the precise property and the
bounded tool-class assumptions it does carry are defined in
[DOC-5 §2.2](./05-controller-cli.md)) — it operates entirely through a **versioned
per-tool contract** (DOC-1) and a **stack manifest/BOM** that doubles as its tool
registry (DOC-2).

This design set decomposes that vision into 11 focused, mutually-consistent
design docs. Each refines one concern of the spec into an implementable design.
The set is sequenced so the two foundational contracts (the tool contract, DOC-1;
and the stack manifest, DOC-2) land first, because nearly everything else
consumes them. All docs are now Accepted; the remaining work is implementation,
sequenced in [Implementation order](#implementation-order) below.

## Dependency graph

```
                         DOC-0 (naming & charter)  ──► ADR-0006
                                  │  name `trusty-controller`/`tctl` flows to ALL docs
                                  ▼
              ┌──────────── DOC-1 (tool contract) ◄────► DOC-3 (scope model)
              │             ADR-0007                       (system vs project)
              │                   │                          │  ADR-0008
              │                   ▼                          ▼  project identity
              │            DOC-2 (manifest/BOM)        (scope schema feeds DOC-1)
              │             │   │   │   │   │
              ▼             ▼   ▼   ▼   ▼   ▼
        DOC-6 (conformance + mpm adapter) ◄── ADR-0008 (project-identity hoist)
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

- **DOC-0** (→ ADR-0006) produces the chosen name `trusty-controller`/`tctl`
  consumed by every other doc.
- **DOC-1 ◄──► DOC-3** are bidirectional: DOC-1 owns the wire format of `scope`;
  DOC-3 owns its behavioral model and feeds schema fields back to DOC-1.
- **DOC-1** (→ ADR-0007) produces inputs for DOC-2, DOC-4, DOC-5, DOC-6, DOC-7, DOC-9.
- **DOC-2** produces inputs for DOC-5, DOC-6, DOC-7, DOC-8, DOC-9.
- **DOC-3** (→ ADR-0008) produces inputs for DOC-1, DOC-4, DOC-5, DOC-6, DOC-8.
- **DOC-4** produces inputs for DOC-5, DOC-7, DOC-10.
- **DOC-5** produces inputs for DOC-7, DOC-8, DOC-9, DOC-10.
- **DOC-6** gates DOC-10 (only conformant tools can be rolled up/tested) and
  produces inputs for DOC-4, DOC-5.
- **DOC-7, DOC-10** are terminal (nothing downstream depends on them).
- **DOC-8, DOC-9** produce inputs for DOC-10.

## Document index

All 11 docs are **Accepted (owner-approved)**.

| Doc | Title | One-line purpose | Status | ADR |
|---|---|---|---|---|
| [**DOC-0**](./00-naming-and-doc-charter.md) | Naming & Documentation Charter | Lock the tool's real name (binary/crate/dir) and doc conventions. | Accepted | [ADR-0006](../../../adr/0006-trusty-controller-naming.md) |
| [**DOC-1**](./01-tool-contract.md) | The Versioned Tool Contract | Define the exact versioned interface every stack member implements (FOUNDATIONAL). | Accepted | [ADR-0007](../../../adr/0007-tool-contract-versioning-and-verb-model.md) |
| [**DOC-2**](./02-stack-manifest-and-versioning.md) | Stack Manifest/BOM + Version & Changelog Advertisement | Define the manifest/BOM/lockfile + tool registry + structured changelogs (FOUNDATIONAL). | Accepted | — |
| [**DOC-3**](./03-scope-model.md) | Scope Model (System vs Project) | Specify the behavioral model behind the scope axis (readiness, idempotency, blast radius, project identity). | Accepted | [ADR-0008](../../../adr/0008-project-identity-convention.md) |
| [**DOC-4**](./04-doctor-health-rollup.md) | Doctor/Health Rollup Model | Aggregate per-tool doctor/health JSON into a stack verdict + tools×scope matrix. | Accepted | — |
| [**DOC-5**](./05-controller-cli.md) | Controller CLI Command Surface + Dispatch | Define the controller's own CLI and manifest-driven verb dispatch. | Accepted | — |
| [**DOC-6**](./06-contract-conformance-and-mpm-adapter.md) | Per-Tool Contract Conformance + claude-mpm Python Adapter | Audit each tool vs DOC-1; specify how Python claude-mpm satisfies the contract. | Accepted | — |
| [**DOC-7**](./07-controller-web-ui.md) | Controller Web UI (link-out control plane) | Out-of-the-box UI that links out to each tool's existing UI, never reimplementing. | Accepted | — |
| [**DOC-8**](./08-install-bootstrap.md) | Install/Bootstrap Flow (UUC1, UUC2) | Zero-knowledge install + per-project auto-config on claude-mpm launch. | Accepted | — |
| [**DOC-9**](./09-upgrade-flow.md) | Upgrade Flow (UUC3) | Cross-tool update detection, changelog headlines, upgrade + take-effect restart. | Accepted | — |
| [**DOC-10**](./10-isolation-testing-harness.md) | Isolation Testing Harness (MUC1, MUC2) | Test stack install/upgrade in a vanilla container/VM without contaminating the host. | Accepted | — |
| [**DOC-11**](./DOC-11-open-issues.md) | Open Issues & Adversarial-Review Tracker | Living tracker of open issues raised against the Accepted design set; iterated per item. | Active tracker | — |

## Implementation order

The design set is complete; what follows is the recommended **implementation
spine** — the order that respects the dependency graph and lands the
load-bearing contracts before the surfaces that consume them.

1. **Build the contract module** — land `trusty_common::contract` (DOC-1 D6): the
   uniform envelope, `contract_version`, the `verbs[]` capability descriptor, and
   the dispatcher all Rust tools share. Everything downstream depends on it.
2. **Hoist project identity into `trusty_common`** — promote `detect_project` /
   `id_from_path` (full-path-slug scheme) to a shared home so every tool resolves
   the same project id (ADR-0008 / DOC-6 Q9; DOC-3 §8).
3. **Per-tool conformance retrofits** — retrofit tools against the shared module
   in dependency order: **trusty-search → trusty-memory/trusty-analyze →
   trusty-review** (the laggard / heaviest retrofit) (DOC-6 §2).
4. **claude-mpm uv shim** — give the external Python orchestrator a contract
   adapter invoked via the `uv` shim so it advertises the same envelope/verbs
   (DOC-6).
5. **The `trusty-controller`/`tctl` crate** — manifest loading (DOC-2), verb
   dispatch + passthrough (DOC-5), and the doctor/health rollup (DOC-4).
6. **Web UI** — the loopback-only link-out control plane (DOC-7).
7. **Install / upgrade flows** — the UUC1/UUC2 install + bootstrap (DOC-8) and
   the UUC3 upgrade flow (DOC-9), composing existing `trusty_common::{update,
   launchd, shutdown}` primitives.
8. **Isolation harness** — the vanilla container/VM install+upgrade test
   harness that exercises the whole stack end-to-end (DOC-10).

## Key decisions digest

A condensed read of the locked cross-cutting decisions, so a reader gets the gist
without opening every doc. Each reflects what the Accepted docs actually say.

- **Naming & publishing (DOC-0 / ADR-0006)** — crate `trusty-controller`
  (`crates/trusty-controller/`), binary + alias `tctl`, Elastic License 2.0,
  published to crates.io.
- **Tool contract (DOC-1 / ADR-0007)** — a **monotonic-integer
  `contract_version`** (starts at `1`, a single negotiated capability level, not
  semver); a **uniform response envelope** (same outer JSON for every verb, only
  `data` varies); a **capability `verbs[]`** descriptor the controller discovers
  at runtime (missing verbs → graceful degrade; unknown verbs → ignored); and
  **generic passthrough** (`tctl <tool> <verb> [args]` forwards any advertised
  verb). Contract types live in a new `trusty_common::contract` module (D6).
- **Scope & project identity (DOC-3 / ADR-0008)** — two scopes, **system vs
  project**; the canonical project id is the **full-path slug** of the git root
  (`id_from_path`), falling back to a cwd path-slug with a warning when no root or
  marker is found.
- **Stack manifest/BOM (DOC-2)** — **TOML** (matches the repo's hand-authored
  config convention), shipped as an **embedded default** compiled into `tctl`
  with an optional **system-level override** file
  (`~/.config/trusty-controller/manifest.toml`) that wins when present;
  `stack_version = "YYYY.MM-N"` (e.g. `2026.06-1`); changelog headlines extracted
  best-effort from per-tool Keep-a-Changelog sources.
- **Orchestrator is pluggable (DOC-0 A4 / DOC-6)** — the orchestrator is a
  swappable stack member: **claude-mpm** (Python, external, via a contract
  adapter behind a `uv` shim) is the current stable orchestrator;
  **trusty-mpm** (Rust, in-house) is the planned replacement. The contract +
  manifest treat it as swappable with no controller changes.
- **Web UI is a control plane, not a data plane (DOC-7)** — the UI **links out**
  to each tool's existing UI rather than reimplementing it, and is
  **loopback-only** with no auth (matching the daemons).
- **Install/upgrade reuse (DOC-8 / DOC-9)** — both flows compose existing,
  already-grounded `trusty_common::{update, launchd, shutdown}` primitives
  (`perform_upgrade`, `check_crates_io`, `bootout`/`bootstrap`, the graceful-drain
  shutdown from #534) rather than introducing new install machinery.
- **Security / secrets (DOC-1, DOC-2, DOC-7)** — no secrets in contract output or
  in the manifest; daemons and the UI are loopback-only with no auth.

## Related ADRs

- [ADR-0006 — trusty-controller naming](../../../adr/0006-trusty-controller-naming.md) (DOC-0)
- [ADR-0007 — Tool contract versioning + verb model](../../../adr/0007-tool-contract-versioning-and-verb-model.md) (DOC-1)
- [ADR-0008 — Project identity convention](../../../adr/0008-project-identity-convention.md) (DOC-3)
