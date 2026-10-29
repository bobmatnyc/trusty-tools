Fixed
- `AllowlistConfig::default_path` now honours `TRUSTY_DATA_DIR` (non-empty, absolute) and returns `$TRUSTY_DATA_DIR/allowlist.toml`; `load()` skips the legacy `indexes.toml` migration in that case, so an isolated daemon never touches the operator's files (#10366).
