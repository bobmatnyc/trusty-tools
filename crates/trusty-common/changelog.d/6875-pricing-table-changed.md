Changed

- `toml` is now an unconditional dependency, since the `pricing` module parses
  the bundled table on every build. The `tickets` and `credentials` features no
  longer name `dep:toml`. (#6875)
- `TRUSTY_TOOLS_DIR` is defined once at the crate root; the identical constants
  in `crate_config` and `palace_resolve` are now re-exports of it. Both paths and
  the value are unchanged. (#6875)
