Added

- `tm doctor` row `bundled_asset_lag`: warns when the running binary's
  compile-time-embedded skill assets differ from the `origin/main` source tree
  they were built from, naming the lagging files, the binary's build timestamp
  and the newest asset commit's. Applies only in `bobmatnyc/trusty-tools`;
  reports UNKNOWN — never a pass — when the source tree cannot be read.
  `skill_staleness` compares deployed files against those same embedded assets
  (#4604) and is structurally unable to see this. Refs #8482.
- `binary_provenance` no longer asserts "the binary is NOT stale" from a semver
  comparison against cargo's registry ledger; the claim is scoped to what it
  reads and points at `bundled_asset_lag`. Refs #8482.
