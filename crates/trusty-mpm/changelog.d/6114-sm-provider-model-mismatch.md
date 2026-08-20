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
  never which model the operator asked for. Explicit `anthropic/` / `bedrock/` /
  `openrouter/` prefixes still win, and are still stripped before the shape is
  read. The function is now fallible.
