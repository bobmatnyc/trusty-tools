Changed
- Tests now cover tm's settings-backup excusal on both decommission routes (`--force` in-project worktree and owned workspace): unedited tm backups are removed, while edited, foreign, unrecorded or corrupt-ledger backups keep the tree and are named; no behaviour change (#11267).
