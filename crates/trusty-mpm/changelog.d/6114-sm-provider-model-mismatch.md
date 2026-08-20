Fixed

- The Session Manager no longer sends a model id to a provider that has never
  heard of it. A tier model with no routing prefix took the configured
  `provider` whatever it looked like, so a `us.anthropic.*` inference profile
  under `provider = "openrouter"` — or an OpenRouter slug under an `auto` chain
  that picked Anthropic — went upstream unchanged.
  `core::sm::providers::resolve_provider_and_model` now infers the provider from
  an unambiguous id shape before the default or the credential precedence chain
  gets it, and returns `SmLlmError::Validation` naming both the id and the
  provider when they disagree. Credentials decide which provider is reachable,
  never which model the operator asked for. An explicit `anthropic/` /
  `bedrock/` / `openrouter/` prefix still wins, is still stripped before the
  shape is read, and is overruled only by a shape that belongs to exactly one
  provider's catalogue — never by the dotted `vendor.model` guess, so
  `openrouter/anthropic.claude-x` keeps working. The function is now fallible.
- A rejected model id no longer suggests a `provider` value the config parser
  refuses. When the id belongs to a provider the SM has no client for, the error
  says so and names the accepted set instead of telling the operator to set
  `provider = "fireworks"`. `ProviderKind::parse`, its error, and the
  shape-mismatch remedy all read one list (`ProviderKind::ALL`), so the accepted
  values and the routing-prefix hint cannot drift from what the parser takes.
