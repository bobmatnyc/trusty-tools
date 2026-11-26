Changed
- `trusty-memory setup` writes the Codex registration under the configured Codex home and refuses the write on a broken `install.toml`; `doctor` reads the same file and reports a broken `install.toml` as `Unknown` (#11821).
