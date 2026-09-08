Added

- `taudit audit` now checks, before anything is cloned, whether the OPTIONAL
  collector binaries (`gitleaks`, `cargo-audit`, `cargo-deny`) are on this
  machine — reusing the exact `trusty_common::bin_resolve::resolve_binary`
  lookup each collector already calls at collection time. By default a missing
  one only warns: the sweep still runs, and `ChainReport::collector_gaps`
  carries one row per gap naming the collector, the evidence dimension that
  goes dark, and the install hint, printed ahead of everything else in the
  chain's report. `--strict-collectors` turns that warning into a refusal —
  `AuditError::MissingOptionalCollectors`, attributed to the new
  `chain::Phase::Preflight` — before the first repository is cloned. Neither
  mode changes each repository's own `[report].gaps` line for the same
  collector, which is unaffected either way.
