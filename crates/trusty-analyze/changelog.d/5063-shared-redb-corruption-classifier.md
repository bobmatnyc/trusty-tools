Changed

- `core::redb_open::is_format_obsolete` now delegates to
  `trusty_common::redb_open::is_incompatible_format` instead of carrying its own
  copy of the four-arm `match` (#5063). Same verdict for every input; the
  quarantine policy `open_or_quarantine` is unchanged and stays here, because it
  takes a caller-supplied suffix and recovery string that trusty-common's
  fixed-suffix helper does not offer.
