# Tart VM Testing Harness — Research

**Date:** 2026-07-31

This directory holds a research effort into using local Tart macOS VMs for isolated
installation testing of the trusty-tools stack. Research is **COMPLETE**; **no harness
has been built**. The work proceeded as two independent research tracks, a fact-check
reconciliation pass between them, an empirical probe over Tart VM behavior (sections
A–K in `vm-install-probe-findings.md`), and — after an adversarial review surfaced
untested assumptions — a decisive measurement pass that falsified the original
build-cost model by roughly 20x and killed the golden-image strategy.

<!-- TODO: follow-up adds four analysis documents (fact-check-reconciliation, collated-recommendation, devils-advocate-review, conclusions-post-measurement) and per-file status markers -->

## Contents

- **[vm-install-probe-findings.md](./vm-install-probe-findings.md)** — the empirical
  probe pass (sections A–K): base image inventory, `tart exec` viability, TCC/permission
  behavior, mise + Rust reality check, `tart stop`/`suspend` failure modes, and the
  final build-cost measurement pass (`PROVISION_SEC`, from-source `cargo install`
  timing, LTO/ONNX verification).
- **[vm-install-testing-trackA-fable.md](./vm-install-testing-trackA-fable.md)** —
  Track A parallel research: independent design/recommendation pass for a Tart-based
  install-testing harness (no code changes, no VMs started).
- **[vm-install-testing-trackB-opus.md](./vm-install-testing-trackB-opus.md)** —
  Track B parallel research: independent design/recommendation pass, produced without
  coordination with Track A, for the same harness question.
- **[artifacts/bake-golden.sh.superseded](./artifacts/bake-golden.sh.superseded)** —
  the golden-image bake script that produced measurements A–K; superseded because the
  golden-image strategy it implements was killed by measurement. See the header comment
  for the full rationale. Not to be used as the basis for the real harness.
- **[logs/](./logs/)** — raw logs and extracted artifacts from the K measurement pass
  (`tart stop` asynchrony, `PROVISION_SEC`, and the `trusty-search` from-source build).
  See `logs/README.md` for the directory note and `vm-install-probe-findings.md`
  section K for the writeup.

## Related

- PR #4438 amends `docs/trusty-installer/research/02-design/10-isolation-testing-harness.md`
  with the outcome of this research.
