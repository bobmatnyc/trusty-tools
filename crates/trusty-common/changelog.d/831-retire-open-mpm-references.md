Fixed

- Updated doc comments and Cargo.toml that still named `open-mpm` as a
  consumer crate to say `trusty-agents` (renamed in #831), and corrected the
  `symgraph::SymbolRegistry` on-disk path from the stale, hardcoded
  `.open-mpm/state/symbol-registry.json` to `.trusty-agents/state/…`,
  matching the rest of trusty-agents's config-dir convention. The registry is
  a regenerable content-addressed cache, so existing installs simply rebuild
  it under the new path. Genuine back-compat (`OPEN_MPM_*` env-var fallbacks,
  the `.open-mpm` legacy-dir migration, and the kuzu-memory `.open-mpm/memory`
  migration source) is unchanged.
