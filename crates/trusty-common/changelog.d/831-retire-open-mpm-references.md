Fixed

- Updated doc comments and Cargo.toml that still named `open-mpm` as a
  consumer crate to say `trusty-agents` (renamed in #831), and corrected the
  `symgraph::SymbolRegistry` on-disk path from the stale, hardcoded
  `.open-mpm/state/symbol-registry.json` to `.trusty-agents/state/…`,
  matching the rest of trusty-agents's config-dir convention. The registry is
  a regenerable content-addressed cache, so existing installs simply rebuild
  it under the new path. Genuine back-compat (`OPEN_MPM_*` env-var fallbacks
  and the `.open-mpm` legacy-dir migration) is unchanged. `KuzuSource`'s
  `~/.open-mpm/memory` root is left alone too, but it is not back-compat: it
  has zero callers, its feature is enabled by nobody, and the real migrator
  (`trusty-memory`'s `kuzu_migrate`) takes a mandatory `--from <store.redb>`
  instead of discovering that path.
