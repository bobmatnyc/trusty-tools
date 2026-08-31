Added

- `[providers] default_provider_id` in `~/.trusty-agents/config.toml` decides
  which inference provider serves the bundled agent templates
  ([#3766](https://github.com/bobmatnyc/trusty-tools/issues/3766)). Seven
  bundled templates ship a bare model slug (`claude-sonnet-4-6`) and no
  `[agent].provider_id`, so until now the provider was whichever ambient
  credential the harness found first — the same template ran on
  Anthropic-direct on one machine and OpenRouter on the next, and could change
  provider when an unrelated environment variable appeared. Set the key once
  and every such template resolves that provider deterministically.
  The setting applies only to agents that declare neither a pin nor a provider
  in their model slug: an explicit `[agent].provider_id` and a provider-named
  template (`bedrock/…`, `openai/…`) both win over it. It fails closed like a
  pin — a provider with no resolvable credential aborts the agent load with a
  message naming the config key — and it lives in the operator config rather
  than in the template files, so refreshing a bundled template cannot revert
  it. Leaving the key unset keeps the previous ambient behaviour exactly.
