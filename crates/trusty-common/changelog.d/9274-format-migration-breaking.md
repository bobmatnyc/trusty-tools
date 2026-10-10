Breaking
- A format-0 palace is refused with `FormatNeedsMigration` on every read-only open — `PalaceHandle::open_with_intent(.., OpenIntent::ReadOnlyClient)` and a `PalaceRegistry` left at its default intent — until a writer-intent open migrates it (ADR-0067 D3 rule 2). A registry that is the sole owner of its root must call `with_writer_intent()`.
- `KgStoreRedb::open` and `KgStoreRedb::open_with_intent` refuse a format-0 `kg.redb` under every intent; open the palace through a writer-intent `PalaceHandle` first.
- A `kg.redb` in an incompatible redb format is refused under writer intent, its bytes untouched, instead of being renamed to `*.v2-incompatible` and recreated empty (ADR-0067 D5).
- A palace at format 1 cannot be downgraded: a binary that reads only format 0 refuses it, and no 1 → 0 migration exists. Restore the format-migration backup under `<data_root>/backups/format-migration/<palace>/` to go back.
