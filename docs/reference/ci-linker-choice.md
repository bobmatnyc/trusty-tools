# CI linker choice — measured, rejected

A proposal to speed up CI by switching the linker to `lld` or `mold` was
measured and rejected: **CI already links with `rust-lld`**. This page records
the measurement so the idea is not re-proposed without re-reading it.

## The trap

`ld --version` on a GitHub Linux runner reports GNU ld 2.42. That is
`/usr/bin/ld`, which `rustc` does not invoke, so it is not evidence of what CI
actually links with. This is what produced the original proposal.

## What CI actually links with

`rustc` uses the toolchain-bundled `rust-lld` by default on
`x86_64-unknown-linux-gnu`. Verified from the `.comment` section of a
default-linked binary:

- stable 1.97.1 stamps `Linker: LLD 22.1.6 (/checkout/src/llvm-project/...)`
- MSRV 1.94.1 stamps `LLD 21.1.8`

`/checkout/src/llvm-project` is Rust's own build root, confirming this is
`rust-lld`, not a system install.

## Measurements

18 cold `cargo test --workspace --no-run` runs on `ubuntu-latest`, 391 link
invocations each. Link share: ~12% of the build, ~9% of the `Test` job.

| Linker | Mean job wall | vs. default |
|---|---|---|
| default (`rust-lld`) | 827.5s | — |
| explicit `-fuse-ld=lld` | 800.2s | inside run-to-run spread — same linker either way |
| `-fuse-ld=bfd` (GNU ld) | 1171.7s | **+344s** — the only large effect available, and it's a regression |
| mold | 313.7s vs. lld's 327.2s over 3 paired back-to-back link passes | ~4% faster than lld, plus ~2.5s apt install per job |

Clippy and MSRV legs link almost nothing — they're check-only: 9.2s of 381s
(2.4%) and 11.4s of 372s (3.1%), both smaller than runner noise.
`ci.yml:1101` already carries a comment that `cargo check` never invokes the
linker.

macOS is unaffected either way: Apple's `ld-1267` linked trusty-common's 37
test binaries at ~86ms mean, so no Linux-scoped linker config would do
anything there.

## Two warnings for anyone tempted to revisit this

1. **Do not add a `-fuse-ld` flag to `.cargo/config.toml`.** `release.yml`
   builds Linux targets with `cargo zigbuild`, which installs its own linker,
   and the AL2023 job runs in a container with no system lld. A repo-wide
   `rustflags` entry reaches both of those legs and can make the Test job
   ~40% slower.
2. **A single-sample CI A/B cannot resolve effects below ~30% on these
   runners.** The first attempt at this measurement showed lld "winning" by
   65s — noise: the clippy legs, which link almost nothing, spread from 381s
   to 501s on identical work between runs. Use 4-8 repetitions and account
   for CPU time, not just wall time.

## Where the time actually goes instead

Linking is a small, already-optimal slice. After PR #5366, build is ~78% of
the `Test` job, and linking is ~12% of that build — the remaining bulk is
codegen. The structural inefficiency worth chasing is that four runners each
perform the same full workspace build from scratch; sharing or splitting that
work would beat any linker swap.
