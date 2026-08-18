Added

- `download::pinned::preflight_pinned_set` answers whether a pinned set could be
  installed on this host without installing it — a Tier-1 target check and a
  release-list lookup per tool, and no download, hash, or execution. It shares
  `resolve_pin` with `install_pinned_set`'s staging path, so a preflight cannot
  come to a different answer than the install it precedes, and a consumer that
  needed the question answered no longer has to install or grow a second
  resolver ([#5970](https://github.com/bobmatnyc/trusty-tools/issues/5970))
