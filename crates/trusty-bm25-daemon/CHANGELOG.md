# Changelog — trusty-bm25-daemon

## [0.1.2] — 2026-06-16

### Changed (BREAKING for the binary; closes part of #1318)

- **Library-only.** Removed the `[[bin]]` target / `src/main.rs` shim. The
  `trusty-bm25-daemon` **binary** is now produced solely by `trusty-memory`
  (the host that bundles and spawns it), eliminating the cargo `.crates2.json`
  binary-ownership collision that made `cargo install trusty-memory` fail
  without `--force` (#1262). This crate is still published to crates.io as a
  **library** (`publish = true` retained). Install the binary via
  `cargo install trusty-memory`.

## [Unreleased]

### Changed
- Extracted daemon logic into a `[lib]` target (`src/lib.rs`) with a
  `pub async fn run()` entry point. `src/main.rs` is now a thin shim.
  This is a non-breaking change: the standalone binary behaviour is
  identical; the library target is a new addition.

### Added
- `[lib]` target (`crate-type = ["rlib"]`) enabling bundled-install:
  `trusty-memory`'s `Cargo.toml` now lists `trusty-bm25-daemon` as a
  dependency and adds a `[[bin]]` shim so `cargo install trusty-memory`
  produces the daemon binary alongside the main binary.
