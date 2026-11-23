Fixed

- `tagent --slack` no longer panics on its first WebSocket handshake
  (closes [#4825](https://github.com/bobmatnyc/trusty-tools/issues/4825)).
  A single `rustls 0.23` instance in the dependency graph carries both the
  `ring` and `aws-lc-rs` provider features, so rustls could not auto-select one
  and aborted the process the moment the Socket Mode connector built a
  `ClientConfig` — right after logging "Slack Socket Mode connected". `run()`
  now installs the `aws-lc-rs` provider explicitly before any TLS-capable code
  path, and fails loudly at startup if no provider ends up installed.
- The Slack gateway resolves the project it was actually pointed at
  (closes [#4826](https://github.com/bobmatnyc/trusty-tools/issues/4826)).
  An explicitly-set `TAGENT_PROJECT_DIR` now outranks the `current_exe()`
  walk-up in self-project detection; previously a hint without a
  `.trusty-agents/agents/pm.toml` marker was dropped silently and the walk-up
  from `~/.cargo/bin/tagent` resolved `$HOME`, so a deployed gateway answered
  users against the wrong project instead of failing. Marker-based inference
  remains the fallback for when no hint is given.
