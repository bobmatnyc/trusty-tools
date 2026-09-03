Added

- `tga inspect schema` prints every table, view, and column the database in
  front of you actually holds, with row counts and the free-text columns marked
  (#5218). It reads the file rather than the migration set, because a database
  collected by an older `tga` is missing later tables and nothing in
  `src/core/db/sql/` says so.
- `tga inspect attest` states the data-handling claim — "tga's database stores
  no file content, diffs, patches, hunks, or blobs" — and prints the live
  evidence for it: the scan that found no BLOB and no diff-named column, and a
  per-column reading of every free-text column in that database. The claim is
  never "contains no code", because a pasted snippet in a commit message is
  stored verbatim; the caveat saying so travels with it, and
  `claim_never_says_no_code` fails if a later edit loosens either. DOC-67 §10
  quotes these two strings rather than paraphrasing them.
- `work_items.raw_json` is read at runtime rather than cited from
  `0005_work_items.sql`. Today's writer serializes a struct with no description
  field, but that is a property of the writer, not of the column. The diff probe
  unescapes JSON line breaks before matching, because a diff serialized into
  that column carries the two-character `\n` escape rather than a newline byte —
  four of the five markers anchor on a real line start and would otherwise miss
  the one column the attestation most needs to read. The probe reads plain text
  and that escaping; a base64-encoded or compressed diff reads as opaque text
  and is not counted, which is part of why the "not a claim that the database
  contains no code" caveat is not optional.
- Both subcommands open the database read-only and refuse a missing path, a
  directory, or a non-SQLite file, each naming the cause. The shared
  `Database::open` would have CREATED and migrated a missing file, so an
  inspection routed through it would print a complete, empty, freshly-minted
  schema and exit 0 for a database the caller cannot read. `attest` also exits
  non-zero when its verdict is `findings`, so a hand-over script can gate on it.
- Two standing guards replace one-time reads. `every_text_column_is_classified`
  fails when a migration adds a `TEXT` column that is in none of the three
  inventories in `core::inspect::text_columns`, so the free-text list cannot go
  stale silently. `diff_for_commit_callers_match_the_attestation` re-derives the
  non-test callers of `collect::git::diff::diff_for_commit` from the source tree
  and compares them against the pinned list — #5218 asked for "zero callers",
  which #5465 has since made false, so the enforceable property is now "these
  callers and no others".
