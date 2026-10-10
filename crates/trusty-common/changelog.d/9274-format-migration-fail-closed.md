Changed
- `PALACE_FORMAT_SUPPORTED` is now 1. A new KG is stamped at format 1 when it is created.
- The #4810 triple-key rewrite no longer runs at every KG open and no longer fails open: it is the KG step of the format migration, behind the format backup, and an undecodable legacy row or a triple-key marker above 1 fails the migration instead of being skipped. The `kg.redb.pre-4810.bak` sidecar is no longer written.
- Derived stores keep the #702 recovery of an incompatible redb file.
- The KG open re-reads the format marker on the database it opened on every fresh open, not only when the pre-open read was deferred.
- A format-migration backup is reused only when its manifest matches the palace's current files.
