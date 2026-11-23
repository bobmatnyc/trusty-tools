Fixed

- **`log_drain` module docs resolve their own links again (#7164).** The four
  intra-doc links in the module header (`LogDrainConfig`, `resolve_log_drain`,
  `LogDrainSetting`, `spawn`) are now backed by `crate::log_drain::…`
  reference definitions, so rustdoc resolves them instead of failing the
  intra-doc link gate.
