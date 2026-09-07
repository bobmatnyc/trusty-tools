Documentation

- **`paths` module docs link to their own items again (#7001).** The five
  intra-doc links in the module header (`resolve_project_entry`, `SEARCH_ROOTS`,
  `ConfigSource`, `native_config_dir`, `check_native_write_target`) are now
  written as `crate::paths::…` reference definitions, so rustdoc resolves them
  instead of failing the intra-doc link gate.
