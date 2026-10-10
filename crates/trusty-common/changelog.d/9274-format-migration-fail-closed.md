Changed
- `PALACE_FORMAT_SUPPORTED` is now 1. A read-only open of a format-0 palace, and a direct `KgStoreRedb` open of a format-0 `kg.redb`, are refused with `FormatNeedsMigration` until the daemon migrates the palace (ADR-0067 D3 rule 2). A new KG is stamped at format 1 when it is created.
- The #4810 triple-key rewrite no longer runs at every KG open and no longer fails open: it is the KG step of the format migration, behind the format backup, and an undecodable legacy row or a triple-key marker above 1 fails the migration instead of being skipped. The `kg.redb.pre-4810.bak` sidecar is no longer written.
- A `kg.redb` in an incompatible redb format is refused under writer intent, its bytes untouched, instead of being renamed to `*.v2-incompatible` and recreated empty (ADR-0067 D5). Derived stores keep the #702 recovery.
- The KG open re-reads the format marker on the database it opened on every fresh open, not only when the pre-open read was deferred.
- A format-migration backup is reused only when its manifest matches the palace's current files.
