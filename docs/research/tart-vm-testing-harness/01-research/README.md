# Tart VM Testing Harness — Research

**Date:** 2026-07-31

This directory holds a research effort into using local Tart macOS VMs for isolated
installation testing of the trusty-tools stack. Research is **COMPLETE**; **no harness
has been built**. The work proceeded as two independent research tracks, a fact-check
reconciliation pass between them, an empirical probe over Tart VM behavior (sections
A–K in `vm-install-probe-findings.md`), and — after an adversarial review surfaced
untested assumptions — a decisive measurement pass that falsified the original
build-cost model by roughly 20x and killed the golden-image strategy.

## Reading order

For a newcomer: **README (this file) → `conclusions-post-measurement.md` →
`vm-install-probe-findings.md` → the rest as background.**

## Contents

| Document | Status |
|----------|--------|
| [conclusions-post-measurement.md](./conclusions-post-measurement.md) | **CURRENT — authoritative** |
| [devils-advocate-review.md](./devils-advocate-review.md) | CURRENT |
| [fact-check-reconciliation.md](./fact-check-reconciliation.md) | CURRENT |
| [vm-install-probe-findings.md](./vm-install-probe-findings.md) | CURRENT (raw measurements A–K) |
| [collated-recommendation.md](./collated-recommendation.md) | SUPERSEDED |
| [vm-install-testing-trackA-fable.md](./vm-install-testing-trackA-fable.md) | SUPERSEDED (initial parallel research track) |
| [vm-install-testing-trackB-opus.md](./vm-install-testing-trackB-opus.md) | SUPERSEDED (initial parallel research track) |
| [artifacts/bake-golden.sh.superseded](./artifacts/bake-golden.sh.superseded) | SUPERSEDED instrument |
| [logs/](./logs/) | raw measurement logs |

- **[conclusions-post-measurement.md](./conclusions-post-measurement.md)** — **CURRENT,
  authoritative.** Final recommendation after the decisive measurement pass: the
  20–55 min/crate cost model is falsified, golden image and single-crate scope are both
  killed, guest vCPU count is the dominant lever, and `tart exec` is the sole
  transport.
- **[devils-advocate-review.md](./devils-advocate-review.md)** — CURRENT. Adversarial
  review of the collated recommendation's nine load-bearing claims against the probe's
  empirical findings; several claims survive only on unmeasured grounds or with their
  cited evidence overturned.
- **[fact-check-reconciliation.md](./fact-check-reconciliation.md)** — CURRENT.
  Independent verification pass resolving factual divergences between research tracks A
  and B (Rust MSRV pin, `tart exec` flags, the owner-approved DOC-10 prior art,
  `tctl install`'s scope trap, `build.rs` source-tree writes, authoritative binary
  names, repo visibility).
- **[vm-install-probe-findings.md](./vm-install-probe-findings.md)** — CURRENT. The
  empirical probe pass (sections A–K): base image inventory, `tart exec` viability,
  TCC/permission behavior, mise + Rust reality check, `tart stop`/`suspend` failure
  modes, and the final build-cost measurement pass (`PROVISION_SEC`, from-source
  `cargo install` timing, LTO/ONNX verification).
- **[collated-recommendation.md](./collated-recommendation.md)** — SUPERSEDED by
  measurement; retained for the decision record. Synthesis of tracks A and B plus the
  fact-check pass, written before the decisive measurement pass.
- **[vm-install-testing-trackA-fable.md](./vm-install-testing-trackA-fable.md)** —
  SUPERSEDED. Track A parallel research: independent design/recommendation pass for a
  Tart-based install-testing harness (no code changes, no VMs started).
- **[vm-install-testing-trackB-opus.md](./vm-install-testing-trackB-opus.md)** —
  SUPERSEDED. Track B parallel research: independent design/recommendation pass,
  produced without coordination with Track A, for the same harness question.
- **[artifacts/bake-golden.sh.superseded](./artifacts/bake-golden.sh.superseded)** —
  SUPERSEDED instrument. The golden-image bake script that produced measurements A–K;
  superseded because the golden-image strategy it implements was killed by measurement.
  See the header comment for the full rationale. Not to be used as the basis for the
  real harness.
- **[logs/](./logs/)** — raw logs and extracted artifacts from the K measurement pass
  (`tart stop` asynchrony, `PROVISION_SEC`, and the `trusty-search` from-source build).
  See `logs/README.md` for the directory note and `vm-install-probe-findings.md`
  section K for the writeup.

## Related

- PR #4438 amends `docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`
  with the outcome of this research.
