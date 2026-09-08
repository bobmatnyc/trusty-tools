Added

- `taudit audit` now checks, before anything is cloned, whether the OPTIONAL
  collector binaries (`gitleaks`, `cargo-audit`, `cargo-deny`) are on this
  machine — reusing the exact `trusty_common::bin_resolve::resolve_binary`
  lookup each collector already calls at collection time. Each missing one is
  narrated on stderr through the existing progress sink, as `Operation::Preflight`,
  before Phase 2 clones anything — not only in the final report. By default a
  missing collector only warns: the sweep still runs, `ChainReport::collector_gaps`
  carries one row per gap naming the collector, the evidence dimension that
  goes dark, and the install hint, and the same rows land in the assembled
  package's `package.toml` (`collector_gaps`), so the recipient who only opens
  the zip sees them too. `--strict-collectors` turns the warning into a
  refusal — `AuditError::MissingOptionalCollectors`, carrying the same
  collector/dimension/install-hint detail as the warning rows, attributed to
  the new `chain::Phase::Preflight` — before the first repository is cloned.
  Neither mode changes each repository's own `[report].gaps` line for the same
  collector, which is unaffected either way.
