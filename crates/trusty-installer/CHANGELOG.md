# Changelog — trusty-installer

All notable changes to trusty-installer are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.4.2] — 2026-07-17

Ships the real Developer-ID signed-install path — the working `tctl sign`
implementation that replaces the previous stub — into the wild (root cause
of #2834's stale-binary report).

### Added

- Unified Developer-ID signed install for search + tm, with plist bootstrap ([#2657](https://github.com/bobmatnyc/trusty-tools/pull/2657)) ([`7f249b3`](https://github.com/bobmatnyc/trusty-tools/commit/7f249b31350647d95cc3c28ae433ab78424a0e1c))
- Turnkey launchd bootstrap for the shared daemon set (closes #2557, #2556) ([#2566](https://github.com/bobmatnyc/trusty-tools/pull/2566)) ([`f47b428`](https://github.com/bobmatnyc/trusty-tools/commit/f47b4286aeb42d0a5871939edf0019a70cdfab78))

### Fixed

- Sign the `tm` binary too so App-Data TCC grant survives reinstalls (closes #2721) ([#2736](https://github.com/bobmatnyc/trusty-tools/pull/2736)) ([`95426c1`](https://github.com/bobmatnyc/trusty-tools/commit/95426c13bdcbf7dc61ba0f66703de0039fb2637b))
- arm64 Tier-1 + glibc-aware ORT asset selection for clean-env install ([#2282](https://github.com/bobmatnyc/trusty-tools/pull/2282)) ([`f9befad`](https://github.com/bobmatnyc/trusty-tools/commit/f9befad04fdf5cf9a93fefefa1e754aaa097c472))

### Changed

- Convert closed-set literals to typed constructs (PR 1: zero-behavior batch) ([#2704](https://github.com/bobmatnyc/trusty-tools/pull/2704)) ([`3b65103`](https://github.com/bobmatnyc/trusty-tools/commit/3b651033f92e619c65bb1aaa77168213e3306b4b))
- Mount config command on all 10 primary binaries ([#2528](https://github.com/bobmatnyc/trusty-tools/pull/2528)) ([`a58ea52`](https://github.com/bobmatnyc/trusty-tools/commit/a58ea5223167553f0d90fb5258d582d510dca316))

### Documentation

- Add missing package metadata to 7 crates ([#2293](https://github.com/bobmatnyc/trusty-tools/pull/2293)) ([`ee58b6a`](https://github.com/bobmatnyc/trusty-tools/commit/ee58b6a4ae01e1338e4761aaa5c27053c49f192b))

## [0.4.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).
- Version reconcile: jumped 0.3.0 → 0.4.1, intentionally skipping past an
  already-published 0.4.0 that came from an orphaned/bad release commit.
