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

No `mise.toml` in repo; Rust pin is 1.91 not stable; ~~`cargo install trusty-mpm`
impossible (`publish = false`)~~ **[FALSE — see the correction below]**;
`cargo install tga` not `trusty-git-analytics`;
`tart exec` has no `--env` and no file transfer; repo is public so no GH token needed.

> **CORRECTION 2026-08-04 (plan P8-T5). The `publish = false` claim about
> `trusty-mpm` is FALSE and was false when it was written.** It is struck above
> rather than deleted, because this file records what the fact-check said at the
> time and a record whose history is rewritten is a record nobody can audit.
>
> `crates/trusty-mpm/Cargo.toml` has **no `publish` key at all**, so cargo defaults
> to `publish = true`. The crate is published: `cargo search trusty-mpm` returned
> `1.0.2` on 2026-07-31 and **1.3.4** on 2026-08-04. DOC-1 D2 was reversed on
> 2026-07-31 on this finding, and Phase 7 of the implementation plan then **proved
> it by execution rather than by a `cargo search`**: a pattern-(a) VM run on
> 2026-08-04 installed it with `cargo install trusty-mpm --locked` from crates.io,
> landed both `tm` and `trusty-mpm`, and `tctl stack doctor` reported
> `trusty-mpm on_path=true version=1.3.4`.
>
> **Everything downstream of the false premise is affected**: pattern (a) covers
> **eight** crates, not six or seven, and a pattern-(a) run that does **not** find
> `tm` is a FAILURE, where under the superseded premise it was the expected result.

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
