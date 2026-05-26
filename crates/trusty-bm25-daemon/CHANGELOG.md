# Changelog — trusty-bm25-daemon

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
