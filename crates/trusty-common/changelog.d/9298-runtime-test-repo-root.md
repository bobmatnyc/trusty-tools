Added
- `test_harness::test_repo_root()` and the pure `resolve_repo_root()` name the Cargo workspace a test reads repository content from, at runtime: `TRUSTY_TEST_REPO_ROOT` (`test_harness::REPO_ROOT_ENV`) first, then the runtime `CARGO_MANIFEST_DIR`, then the current directory, each walked up to the `[workspace]` manifest. It never falls back to a compile-time path (#9298).
