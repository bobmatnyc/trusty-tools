Changed
- `tagent` now carries four verbs: `--version`, `--help`, `config` (and `config profile`) and `mcp-serve`. With no argument it prints the usage text instead of starting the REPL, and any removed mode flag fails with a usage error naming #11632 (#11632).
- Dependencies dropped from the normal build include `axum` (now test-only), `tower-http`, `ratatui`, `crossterm`, `git2`, `teloxide`, `rust-embed`, the `tree-sitter` grammars and the AWS SDK, and the trusty-common features the harness used (#11632).
