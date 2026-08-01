Fixed

- Session launch and `tm sessions sync-assets` now quarantine untracked project-tier agent files that shadow a bundled agent name, immediately after the #4409 workspace retraction. Retraction removes only ledger-tracked entries, so a claude-mpm-era `qa.md` that no manifest names survived every sweep while still outranking the canonical roster — every delegation to that agent silently loaded the stale definition. Offending files are RENAMED to `<file>.md.disabled` (never deleted) with a `TRUSTY-MPM-QUARANTINE.txt` receipt beside them carrying the `mv` that undoes it. A project's own agents and any file the ledger records as the operator's are never touched, and a corrupt ledger or unbuildable roster refuses the whole sweep rather than guessing ([#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448)).
- `SyncAssetsReport` gained `agents_quarantined` and `quarantine_error` so the CLI and daemon route surface the sweep instead of leaving it to a log line — the deployer's equivalent warning fired on eight separate days through 2026-07-30 with no operator follow-up ([#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448)).

Changed

- The bundled-agent name roster moved to `core::bundled_roster`, shared by `tm doctor`'s `asset_tier` probe and the new quarantine. The probe reports exactly what the sweep moves, so a roster built differently at the two sites would have made doctor flag files the sweep skips, or the sweep move files doctor never flagged ([#4448](https://github.com/bobmatnyc/trusty-tools/issues/4448)).
