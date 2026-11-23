Changed

- `memory::redb_recovery`'s format classifier now delegates to
  `trusty_common::redb_open::is_incompatible_format` instead of carrying its own
  copy of the four-arm `match` (#5063). Same verdict for every input;
  `open_redb_or_recreate` is unchanged and stays here, because it returns
  `anyhow::Result` with per-path context and renames to a fixed
  `.v2-incompatible` sibling with no numbered fallback.
