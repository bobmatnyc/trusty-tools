Breaking

- `audit` stops re-exporting `DEFAULT_ANALYZE_PORT`, `DEFAULT_ANALYZE_URL` and
  `ENV_ANALYZE_URL`. All three named a TCP endpoint that no longer exists;
  `default_analyze_socket` and `ENV_ANALYZE_SOCKET` replace them.
- `AnalyzeGuard::from_env` returns `anyhow::Result<Self>` instead of `Self`. A
  derived socket path can fail to resolve where a string literal could not, and
  guessing one would send the audit at a socket the daemon never binds.
- `AnalyzeGuard::socket` replaces its `url` field.
- These move the version to 4.0.0 rather than 3.4.0. `tga` is past 1.0, so the
  breaking position is MAJOR — unlike the 0.x crates in this workspace, where it
  is MINOR. `cargo-semver-checks` hard-stops the publish at any lesser bump.
