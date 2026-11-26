Added
- `tm paths show` prints `projects_root`, `config_dir`, `claude_config_dir` and `codex_home` with the source that won (`env`, `install-file`, `config-template` or `default`). A broken `~/.trusty-tools/install.toml` prints its error and exits non-zero; no `$HOME` default is shown (#11821).
- `tm paths set --projects-root <abs> --config-dir <abs>` writes `install.toml` through the one writer, keeping the file's other keys. A relative or empty path is refused. It refuses inside a Claude Code session or an agent, while any tm session is live, when tmux cannot tell, and over a broken file. It never moves data (#11821).
- The pm-guard floor denies `tm paths set` for every hooked session, the Architect included, under either bypass; `tm paths show` passes (#11821).
