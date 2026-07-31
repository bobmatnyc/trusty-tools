# Collated recommendation

> **SUPERSEDED by measurement.** Retained for the decision record only. Read
> [`conclusions-post-measurement.md`](./conclusions-post-measurement.md) instead — it
> reflects the decisive measurement pass that falsified several claims below (most
> notably the build-cost model and the golden-image economics).

**Status:** SUPERSEDED by measurement — retained for the decision record.
**Date:** 2026-07-31
**Provenance:** synthesis of research tracks A and B plus the fact-check pass, written
before the decisive measurement pass.

## Consensus (both tracks independently)

1. Reject Cirrus CLI, use raw tart + bash.
2. Harness in bash under `scripts/vmtest/`, never `crates/` because
   `members = ["crates/*"]` sweeps it into `cargo test --workspace`, clippy
   `-D warnings`, the 500-SLOC `.rs` line cap, test-pointer/SLD lints and
   publish-guard.
3. Three-stage images `tahoe-base` → golden → per-run CoW clone, with a purity
   assertion.
4. Never RW-mount the host repo.
5. `tart exec` exists and the guest agent is preinstalled.

## Corrections from the fact-check

No `mise.toml` in repo; Rust pin is 1.91 not stable; `cargo install trusty-mpm`
impossible (`publish = false`); `cargo install tga` not `trusty-git-analytics`;
`tart exec` has no `--env` and no file transfer; repo is public so no GH token needed.

## The nine load-bearing claims as stated at the time

1. No Cirrus, partly justified by a macOS 15+ Local Network permission warning in the
   cirrus-cli README.
2. Bash under `scripts/vmtest/`.
3. Three-stage golden image saving 3–12 min/run.
4. Never mount host repo, citing five `build.rs` files.
5. Single-crate default scope, from 20–55 min/crate and 45–90 min full-stack
   estimates.
6. Pattern (a) prebuilt tarballs as daily driver at 5–10 min.
7. Transport undecided, hybrid `tart exec` + SSH.
8. Rust pinned 1.91 with stable as a second dimension.
9. Local transfer by tar of `git ls-files -co --exclude-standard`, repo tree 29 GB but
   `git archive` ~81 MiB / 5,306 files.
