Changed

- `store::redb_error_is_incompatible_format` now delegates to
  `trusty_common::redb_open::is_incompatible_format` instead of carrying its own
  copy of the four-arm `match` (#5063). Same verdict for every input; the dedup
  store's recovery policy is unchanged and stays in `store::dedup_open`, because
  it must serialise the rename-aside behind the sidecar recovery lock (#5064).
