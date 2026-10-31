Changed
- `exclude_globs` now match the path relative to the index root, for every index, not only `std-v1`. A checkout under a directory named `archive` or `.worktrees` no longer loses its files to a glob that named a directory above the root. A glob that starts with `/` still matches the absolute path. A relative glob that embeds the root's own directory name (`proj/src/**`) no longer matches.
- `PATCH /indexes/:id/config` with `profile` or `exclude_globs` now answers `409` while a reindex or deferred embed pass holds the index, and changes nothing. Retry once it completes.
