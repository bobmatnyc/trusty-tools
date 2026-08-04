# Tart VM Testing Harness — Design

**Status:** ~~Draft — design specification only, **no implementation**~~
**IMPLEMENTED — `vmtest-harness/` exists and all three patterns are green.**
*(Amended 2026-08-04, plan P8-T5.)*
**Sibling:** [`../01-research/`](../01-research/) — the research and measurement layer this design consumes
**Parent design:** [`docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`](../../../trusty-installer/research/02-design/10-isolation-testing-harness.md) (Accepted; amended by PR #4438)

This directory refines the completed Tart VM research into an implementable
specification for `vmtest-harness/` — an ad-hoc, manually-run harness that
installs the trusty-tools stack inside a clean local Tart macOS VM and verifies
that the install succeeds without affecting the host.

> **AMENDMENT 2026-08-04 (plan P8-T5) — this line was the doc set's most visible
> stale claim and it is corrected at source.** It read: *"**Nothing here is
> implemented.** `vmtest-harness/` does not exist. These documents specify it."*
> **`vmtest-harness/` now exists**, built over Phases 1–8 of
> [`../03-plan/01-implementation-plan.md`](../03-plan/01-implementation-plan.md),
> and all three patterns have been observed green in real VM runs. These documents
> still **specify** it and remain authoritative for its contracts — but a reader
> arriving here should know the code is there, should read
> [`vmtest-harness/README.md`](../../../../vmtest-harness/README.md) to run it, and
> should read [`../03-plan/MANIFEST.md`](../03-plan/MANIFEST.md) for what each
> phase actually observed — including the contract defects that only execution
> could find, in six of the seven implementation phases.

## Contents

- **[01-vm-install-harness.md](./01-vm-install-harness.md)** — DOC-1. The complete
  harness specification: settled decisions, placement outside the Cargo workspace,
  component architecture, VM run lifecycle, `tart exec` transport rules, the three
  installation patterns, the JSON-only assertion oracle, measurement-backed
  operational constraints, cost baseline, isolation guarantee, extension points,
  non-goals, and known gaps.
- **[02-harness-contracts.md](./02-harness-contracts.md)** — DOC-2. The exact
  interfaces DOC-1 mandates but does not define: the assertion oracle's JSON schema,
  the driver's exit-code contract, base-image digest pinning, `--runid` collision
  prevention, `vmtest clean` semantics, negative-probe mechanics, toolchain path
  hand-off, configuration and tunables, the `expected-binaries.tsv` schema and
  `--check-table` algorithm, polling timeouts, provisioning against a preinstalled
  `mise`, and the scenario↔`lib/` calling convention — plus shell discipline, the
  `jq` dependency, and a traceability table back to DOC-1.

## Reading order

Read [`../01-research/conclusions-post-measurement.md`](../01-research/conclusions-post-measurement.md)
first if you want the *why*; read DOC-1 if you want the *what to build*; read DOC-2
if you are the one building it and need the exact interfaces. Raw
numbers live in [`../01-research/vm-install-probe-findings.md`](../01-research/vm-install-probe-findings.md)
(measurements A–K) and the logs beside it.

## The short version

- **Three patterns:** (c) local source streamed to the guest, (b) branch cloned in
  the guest, (a) released from crates.io. Implementation order is **(c) → (b) →
  (a)**.
- **Eight crates** in scope, and **all three patterns cover all eight** — including
  `trusty-mpm`, published (~~v1.0.2~~ **v1.3.4** as of 2026-08-04) and asserted
  present (D2 reversed 2026-07-31; the old "documented gap in pattern (a)" is
  dissolved), and `trusty-review`, published (~~v0.10.1~~ **v0.11.0** as of
  2026-08-04) and brought into scope by an owner decision the same day (D3
  amended). They produce **thirteen** binaries between them.
  *(Versions refreshed 2026-08-04, P8-T5. **Treat every published version in this
  doc set as illustrative, not as a fixture** — crates.io moves under the docs, and
  both of these moved within four days of being written down. What is load-bearing
  is that all eight are **published and installable**, which Phase 7 proved with a
  real pattern-(a) run rather than with a `cargo search`.)*
- **No golden image.** Clone-and-provision every run — the clone costs 0.31s and
  provisioning 30s, and baking failed three distinct ways in research.
- **The host repo is never mounted**, in either direction. That rule is what closes
  the one real isolation hole.
- **`tart exec` is the sole transport**; never bare-`tart stop`, never
  `tart suspend`.
- **Assertions are JSON-only**, against a checked-in `expected-binaries.tsv` with a
  `--check-table` self-diff mode.

## Related documentation

- [Research layer](../01-research/README.md)
- [Parent design set index](../../../trusty-installer/research/02-design/README.md)

> The `01-research/` directory lands in **PR #4456** (branch
> `docs/tart-vm-research-final`); links to it resolve once that PR merges.
