Fixed

- **A non-empty `default` can no longer disarm the zero-feature test guard.**
  The #4901 guard reads Cargo's `CARGO_FEATURE_*` env vars and discounts
  `CARGO_FEATURE_DEFAULT`, because Cargo activates `default` on every build.
  That discount is correct only while `default` is `[]` — setting
  `default = ["docgen"]` made a bare `cargo test -p trusty-common` look like a
  deliberate feature selection, and the run went green over 407 of the crate's
  ~2062 tests with nothing red anywhere. `build.rs` now reads the manifest and
  a non-empty `default` is a `cfg(test)` `compile_error!`, so the guard says it
  went inert instead of going quiet. `cargo build` / `cargo check` and every
  consumer crate are untouched ([#4901](https://github.com/bobmatnyc/trusty-tools/issues/4901))
- **Which feature sets constitute full coverage is now a manifest fact a test
  checks.** `default = []` and 47 opt-in features mean no single
  `cargo test -p trusty-common` run covers the crate, and which combination
  does was tribal knowledge: `--features inference-client` looked thorough
  while never compiling `inference::bedrock`, and `credentials`,
  `session-naming` and `memory-core` each shipped a PR whose prescribed gate
  never ran their tests. `[package.metadata.trusty-test-coverage]` in
  `Cargo.toml` now names four coverage lanes — `unconditional`, `core`,
  `inference`, `symgraph` — plus five exemptions that each state why no lane
  can run them. `tests/feature_coverage.rs` resolves every lane's transitive
  feature closure from the `[features]` table and fails when the union plus the
  exemptions is not exactly that table, so a feature added without a lane is a
  test failure rather than a silent coverage hole. That test target declares no
  `required-features`, so every invocation runs it — it cannot be gated out by
  the mechanism it polices. `scripts/test_trusty_common_lanes.sh` reads the same
  rows through `cargo metadata` and runs them, so the runner cannot drift from
  the statement ([#4474](https://github.com/bobmatnyc/trusty-tools/issues/4474))
