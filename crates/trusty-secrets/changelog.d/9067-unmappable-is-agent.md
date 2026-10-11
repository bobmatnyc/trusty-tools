Changed
- A bridge assertion with `source: unmappable` must now carry `agent_parent: true`, and `BridgeAssertion::unmappable()` builds it that way: a client the console cannot map is treated as an agent, so `secrets.bridge.set_agents_may_use` ON is `agent_use_refused` and OFF is allowed. `unmappable` with `agent_parent: false` is now `invalid_params` (#9067).
