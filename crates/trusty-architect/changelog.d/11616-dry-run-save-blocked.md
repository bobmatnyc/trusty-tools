Fixed
- `tm architect poll --dry-run` with `ARCHITECT_DRY_RUN_STATE` no longer saves over a state file it could not read, and keeps a `.corrupt.<ts>` copy of a corrupt one before replacing it, as the live poll does.
