Breaking
- Picks up palace format 2 from trusty-common: the daemon migrates each format-1 palace to format 2 at startup, a format-0 palace is refused `FormatTooOld`, and a migrated palace is refused by trusty-memory 0.33.x. `doctor` reports a format-1 palace as needing migration. Rollback: restore the backup, delete `index.usearch.redb`, run `palace_reembed` (#9275).
