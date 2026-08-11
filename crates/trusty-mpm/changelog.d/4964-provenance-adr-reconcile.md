Changed

- `tm doctor`'s `binary_provenance` check splits a ledger/binary version disagreement by direction instead of always reporting `Fail` ([#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964))
  - running binary NEWER than cargo's record at the same path is now `Unknown` — the prebuilt installer placed it and keeps no cargo metadata, so provenance is unverifiable rather than broken
  - running binary OLDER than cargo's record stays `Fail`, unchanged: an older binary on top of a newer install is the [#4033](https://github.com/bobmatnyc/trusty-tools/issues/4033) defect
  - versions that cannot be ordered as semver stay `Fail`
  - `Warn` remains the correct terminal verdict for a `cargo install --path` install per [ADR-0043](docs/adr/0043-cargo-bin-policy.md)
  - the `binary_provenance` description in the bundled `tm-capabilities` skill now states the direction split instead of the retired "any version disagreement fails"
