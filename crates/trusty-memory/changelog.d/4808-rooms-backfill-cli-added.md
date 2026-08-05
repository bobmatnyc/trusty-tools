Added

- `trusty-memory rooms backfill --dry-run | --apply` (ADR-0027 T10) — the
  operator audit path for a migration that runs against live palaces. `--dry-run`
  prints the label each room would be given, by which confidence step, and how
  many drawers sit behind it, then exits WITHOUT writing; `--apply` is required
  to write. It opens palaces directly rather than through the registry,
  deliberately: every registry open path runs the backfill itself, so going
  through it would have written the very rows the dry run exists to preview.
