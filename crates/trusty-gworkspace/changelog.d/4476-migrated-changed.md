Changed

- **BREAKING:** the native binary is renamed from `gworkspace-mcp` to
  `trusty-gworkspace-mcp` (`crates/trusty-gworkspace/Cargo.toml` `[[bin]]`),
  closing #2644. The cargo-installed native binary and the legacy
  pipx-installed Python `gworkspace-mcp` package previously shared one name on
  `$PATH`, so which implementation actually launched depended on install
  order. Migration: update any `.mcp.json` / `~/.claude.json` server entry's
  `"command"` from `"gworkspace-mcp"` to `"trusty-gworkspace-mcp"`, then
  re-run `cargo install --path crates/trusty-gworkspace`. Token/config file
  paths (`~/.gworkspace-mcp/…`) and the default profile name are unchanged —
  existing accounts keep working without re-authenticating.
