Fixed

- **The crate could not be built with `--no-default-features`.** Two files compiled unconditionally while depending on a gated feature: `pipeline/mapreduce/reduce.rs` imported `profile::synthesizer::jaccard_similarity`, and `tests/report_investigate.rs` imported `trusty_review::report`. Both are fixed, and `cargo check -p trusty-review --no-default-features --all-targets` is now clean ([#5466](https://github.com/bobmatnyc/trusty-tools/issues/5466))
  - this was already shipped in 0.15.0 — `cargo add trusty-review --no-default-features --features http-server,mcp` fails against the published crate, which is what `cargo-semver-checks` hit when it tried to build the 0.15.0 baseline
  - `report_investigate.rs` now carries `#![cfg(feature = "report")]`, matching its two siblings `report_e2e.rs` and `report_analyze_e2e.rs`. Under default features all three of its tests still run
