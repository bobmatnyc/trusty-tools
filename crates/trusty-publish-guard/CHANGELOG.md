# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- Initial release: `publish-guard`, the version-parity drift detector ([#3366](https://github.com/bobmatnyc/trusty-tools/issues/3366)).
  - **Core check:** for every publishable workspace crate, compares the local `src/` tree against the crates.io tarball for that crate's OWN current `Cargo.toml` version. A version not yet published is a trivial pass (nothing to compare); a version already live whose source no longer matches is flagged as drift — the exact defect that turned `cargo publish -p trusty-mpm --dry-run` into a release blocker.
  - **Fail-closed:** a registry lookup that errors (rate limit, 5xx, network failure) is treated as a failure, never silently coerced into a pass.
  - **Testable seam:** all crates.io access sits behind the `PublishedFetcher` trait (`src/fetch.rs`); the extraction/diff/decision engine (`src/lib.rs`) is unit-tested against an in-memory fake, no network required.
  - **Wiring:** `scripts/check-version-parity.sh` wrapper and `.github/workflows/version-parity.yml` (push-to-main only, per the issue's own noise-avoidance recommendation).
