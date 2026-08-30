Fixed

- **`GuiState` resolves the daemon URL from an argument, so its tests no longer
  race the process environment.** `cargo test` runs a target's tests as threads
  in one process, and two of them mutated `TRUSTY_CODE_URL`: one set it, the
  other removed it. When the remove landed between the set and the read,
  `env_override_is_trimmed` saw the default URL and failed — on PRs that changed
  neither crate, since `trusty-code-gui clippy` is a required check. `new()` now
  reads the variable once and passes the value to `GuiState::from_url_override`,
  which owns the fallback and the trailing-slash trim without touching the
  environment; both tests call that function directly
  ([#6310](https://github.com/bobmatnyc/trusty-tools/issues/6310))
