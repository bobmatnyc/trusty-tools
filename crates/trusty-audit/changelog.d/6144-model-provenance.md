Added

- The run index's Inference section and every per-repository `manifest.toml`
  now state the endpoint class (`api` or `local`) beside the provider and model
  ids, and the known API host for a provider that has one fixed host
  (`openrouter.ai`, `api.fireworks.ai`) — derived from the resolved provider
  name, never a live lookup and never a credential. `trusty-audit` also bakes
  its build-time git revision into the binary via a new `build.rs`; the run
  index's Versions section states it when the build captured one, replacing
  the previous unconditional "not recorded".
