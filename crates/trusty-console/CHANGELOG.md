# Changelog — trusty-console

All notable changes to trusty-console are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.4.0] — 2026-07-09

### Changed

- Version reconcile to match already-published crates.io state; no functional change.

## [0.3.0] — 2026-06-16

### Changed (closes part of #1318)

- **Sole binary owner.** The standalone `trusty-console` crate is now the ONLY
  producer of the `trusty-console` binary. The bundled `[[bin]]` shims were
  removed from all five host crates (`trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-mpm`, `trusty-review`) to fix the cargo
  `.crates2.json` binary-ownership collisions that forced `--force` on
  `cargo install` / self-`upgrade` (#1262). Install with
  `cargo install trusty-console`.
- **`run()` decoupled from global argv.** Added `run_from(argv: Vec<String>)`
  as the canonical library entry point; `run()` is now a thin wrapper that
  forwards `std::env::args().collect()`. This lets callers (and tests) drive
  the console deterministically without mutating `std::env`.

### Added (closes part of #1318)

- **`trusty-console port [--json]` verb.** Reports the console's bound (live,
  from the discovery file) or default (`7788`) HTTP port. `--json` emits the
  `{"addr":"<host>","port":<u16>}` envelope consumed by `tctl` console
  discovery (`trusty-controller`), fixing the latent bug where `tctl` spawned
  a `port --json` verb that did not exist.
