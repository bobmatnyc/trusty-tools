Added

- `[agent].provider_id` now pins an agent's inference provider for real. The
  key was accepted and persisted by `PATCH /api/agents/:name` but read by
  nothing — `AgentInfo` had no provider field, so the loader dropped it and
  every turn re-probed the ambient environment instead. Setting a provider
  looked like it worked and changed nothing
  ([#3765](https://github.com/bobmatnyc/trusty-tools/issues/3765)).
  A pin takes precedence over whatever provider the model slug implies, and
  fails closed: an unknown provider, a provider chat dispatch cannot reach, or
  a provider with no resolvable credential each abort the agent load with a
  message naming the provider and the env var that would fix it, instead of
  silently falling back to another provider's key. An agent with no pin keeps
  its previous behaviour unchanged.
- AtlasCloud is reachable at dispatch. `atlascloud/*` model slugs previously
  fell through to the OpenRouter endpoint carrying a model id OpenRouter does
  not serve; they now route to `api.atlascloud.ai` with `ATLASCLOUD_API_KEY`.
  `GET /api/models` reports `reachable_today: true` for AtlasCloud and for the
  registry's `local` provider to match.
