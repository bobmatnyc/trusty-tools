Breaking

- `SearchAppState` gained a new public field, `allowlist_paths`, to carry the
  allowlist-gate configuration wired in this release. The struct is not
  `#[non_exhaustive]`, so any external struct-literal construction of
  `SearchAppState` no longer compiles — construct it via `SearchAppState::new`
  and `with_allowlist_paths` instead (#767).
