Security

- `tga audit` now redacts a failed stage's error text before excerpting it, so a credential that spans the 160-character excerpt boundary can no longer survive as a prefix fragment in the generated `manifest.toml` or the due-diligence report handed to a third party. `sweep_gap_lines` takes the credential set as a required argument, and `report::dd_manifest::configured_secrets` is public so the sweep and the manifest builder redact against the same values.
