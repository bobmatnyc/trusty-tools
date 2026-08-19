Fixed
- `tctl self-update` no longer reports success for a binary nothing has run. It
  printed `trusty-installer updated to X. Restart to apply.` and exited 0 as
  soon as the file was placed, so a broken or wrong-version download read as a
  completed update. It now runs `--version` on the exact binary it just wrote
  and cross-checks the reported version, matching the gate `tctl install` has
  always applied to every other member; a failed probe or a mismatch is an error
  and exit 1, never a cargo fallback (#5807).
