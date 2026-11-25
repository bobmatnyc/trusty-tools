Added
- `codex_config::codex_config_path_in` and `codex_config::resolve_codex_config_path`: the Codex config file under the configured Codex home (`CODEX_HOME` or `codex_home` in `install.toml`), else `~/.codex/config.toml`. The resolver form returns the `InstallPathsError` on a broken `install.toml` and never falls back to `~/.codex` (#11821).
