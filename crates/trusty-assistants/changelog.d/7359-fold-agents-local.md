Removed

- The `trusty-agents-local` crate and its binary of the same name (#7359). Its
  `main.rs` was a pass-through to `trusty_agents::run_to_completion()` that
  installed no plugins — byte-for-byte what `tagent` already does — so `tagent`
  is the launcher for everything it used to run. No workspace crate depended on
  it, and `install_plugins` stays a published re-export for downstream launchers.
