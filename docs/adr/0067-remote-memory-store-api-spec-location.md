# 0067. The remote memory-store API spec lives in docs/specs, with wire types in a small dedicated crate

- **Status:** Proposed
- **Date:** 2026-10-04
- **Scope:** Workspace-wide — `docs/specs/`, a new library crate (working name trusty-memory-sync-api), `trusty-memory`, `trusty-mpm`, and any external endpoint implementation
- **Reversibility Cost:** Low before the first external implementation; High after, because third parties would pin the crate and spec location
- **Decision Drivers:** owner ruling 2026-10-04 22:28Z (one interoperable API, interchangeable backends); owner suggestion 22:31Z ("perhaps only the remote memory-store API specifications belong in Trusty-MPM", an option to evaluate); dependency weight for implementers; release cadence
- **Supersedes / Superseded by:** —

## Context

Bob asked for one interoperable memory-sharing API that several backends can implement, and suggested the specification might live in trusty-mpm, with a separate repository for a reference implementation. That is an option to evaluate, not a settled boundary.

What exists: behavior-contract specs live in `docs/specs/` with DOC numbers and SLD anchors (`docs/specs/README.md`). trusty-mpm generates OpenAPI from code for its own daemon API (`crates/trusty-mpm/src/daemon/openapi.rs:1`). trusty-memory publishes an OpenRPC description of its local tools (`crates/trusty-memory/src/openrpc.rs:1-12`). No checked-in OpenAPI document exists.

Options:

| Option | For | Against |
|---|---|---|
| A. Prose spec in `docs/specs/`; wire types and machine-readable schema in a new small crate | Light dependency for connector and third-party endpoints; own semver; spec text follows repository spec conventions | One more crate to publish |
| B. In `trusty-mpm` (crate or its spec folder) | Bob's suggestion; trusty-mpm is the harness that coordinates multi-user sessions | Couples the protocol to the harness's release cadence; the connector in trusty-memory and every endpoint would depend on a large harness crate for types |
| C. In `trusty-memory` | Next to the connector | Endpoints would depend on the whole memory daemon crate |
| D. A separate specification repository | Neutral home for outside implementers | Premature; no outside implementer exists |

## Decision

We will take option A: the normative API text lives in `docs/specs/` (DOC-80 §7 now, its own DOC once stable), and the wire types, OpenAPI 3.x document and JSON Schemas live in a small dedicated library crate in trusty-tools that both the connector and any endpoint depend on. trusty-mpm consumes the crate where it needs it and holds no copy of the contract. If Bob prefers option B, the same content moves under trusty-mpm with no change to the contract itself.

## Consequences

- Implementers pull one small crate; the contract versions independently of trusty-memory and trusty-mpm.
- The conformance suite and fake endpoint (DOC-80 §11) sit beside the crate.
- The crate is published to crates.io before any external endpoint exists, through local-ops.

## Related Decisions

Vetted against prior ADRs on 2026-10-04:

- **ADR-0032 (console is the only HTTP surface):** Consistent — the API is served by remote endpoints, not by a local trusty service.
- **ADR-0065 (trusty-events wire contract in `trusty-common::control_bus`):** Consistent — same pattern of a wire contract in a library crate that producers and consumers share, rather than in one daemon.
- **ADR-0068 (reference implementation repository):** Extends — this ADR places the contract; ADR-0068 places the deployable implementation.
