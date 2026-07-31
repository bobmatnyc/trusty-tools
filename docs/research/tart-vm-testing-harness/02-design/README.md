# Tart VM Testing Harness — Design

**Status:** Draft — design specification only, **no implementation**
**Sibling:** [`../01-research/`](../01-research/) — the research and measurement layer this design consumes
**Parent design:** [`docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`](../../../trusty-installer/research/02-design/10-isolation-testing-harness.md) (Accepted; amended by PR #4438)

This directory refines the completed Tart VM research into an implementable
specification for `vmtest-harness/` — an ad-hoc, manually-run harness that
installs the trusty-tools stack inside a clean local Tart macOS VM and verifies
that the install succeeds without affecting the host.

**Nothing here is implemented.** `vmtest-harness/` does not exist. These documents
specify it.

## Contents

- **[01-vm-install-harness.md](./01-vm-install-harness.md)** — DOC-1. The complete
  harness specification: settled decisions, placement outside the Cargo workspace,
  component architecture, VM run lifecycle, `tart exec` transport rules, the three
  installation patterns, the JSON-only assertion oracle, measurement-backed
  operational constraints, cost baseline, isolation guarantee, extension points,
  non-goals, and known gaps.

## Reading order

Read [`../01-research/conclusions-post-measurement.md`](../01-research/conclusions-post-measurement.md)
first if you want the *why*; read DOC-1 if you want the *what to build*. Raw
numbers live in [`../01-research/vm-install-probe-findings.md`](../01-research/vm-install-probe-findings.md)
(measurements A–K) and the logs beside it.

## The short version

- **Three patterns:** (c) local source streamed to the guest, (b) branch cloned in
  the guest, (a) released from crates.io. Implementation order is **(c) → (b) →
  (a)**.
- **Seven crates** in scope; `trusty-mpm` is a documented gap in pattern (a) only
  (`publish = false`).
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
