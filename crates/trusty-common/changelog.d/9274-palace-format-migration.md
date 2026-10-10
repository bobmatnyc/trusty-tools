Added
- Palace format 1 and the 0 → 1 migration (ADR-0067, #9274). A writer-intent `PalaceHandle` open migrates a format-0 palace at its first open, never in a startup sweep: free-space check, verified backup with its manifest written last, a `format_migration.json` record naming that backup, the `palace.json` mirror, then the triple-key rewrite and both `kg_schema` markers in one redb transaction. A crash or error at any step is completed by the next writer open; a resume trusts only a backup that still verifies.
- `palace_format::palace_format_status` and `PalaceFormatStatus`, a no-write read of a palace's format state for `doctor`, and `format_backup::backup_data_root`.
- `PalaceStoreError::FormatMigrationFailed`, a format refusal naming the palace, both formats and the failed step.
