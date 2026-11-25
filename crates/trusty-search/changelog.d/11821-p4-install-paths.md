Changed
- `trusty-search setup` writes the Codex registration under the configured Codex home and refuses the write on a broken `install.toml`; `doctor` reads the same file and reports a broken `install.toml` as a warning (#11821).
- The allowlist reads the tm project registry from `<config_dir>/.trusty-mpm/project-paths.json` when the install paths move the config dir; `TRUSTY_MPM_ROOT` still wins (#11821).
