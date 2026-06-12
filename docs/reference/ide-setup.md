# IDE Setup Reference

## VS Code / Cursor

- Install `rust-analyzer` extension.
- Install `Even Better TOML` for `Cargo.toml` editing.
- Workspace-level `rust-analyzer` picks up the root `Cargo.toml` automatically;
  no per-crate `.vscode/settings.json` needed.
- Recommended settings in `.vscode/settings.json`:
  ```json
  {
    "rust-analyzer.cargo.features": "all",
    "rust-analyzer.checkOnSave.command": "clippy"
  }
  ```

## RustRover

Open the repo root; it detects the workspace automatically.
