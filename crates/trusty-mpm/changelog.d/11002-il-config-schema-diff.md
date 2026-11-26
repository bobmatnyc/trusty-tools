Added
- The seeded `local/il-schedule.json` and `local/architect.json` carry `schema_version` (ADR-0077 D3). `tm il config diff` compares the schedule with the current preset and marks each difference as a preset change or an edit; `tm il config update` migrates an older schedule one version step at a time, keeping every edit, after a verified backup. A newer `schema_version` is refused (#11002).
