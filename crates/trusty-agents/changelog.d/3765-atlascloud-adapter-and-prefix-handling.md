Fixed

- A keyless AtlasCloud call reported "openrouter credential not found" — the
  wrong provider, env var, and config command. It now names AtlasCloud.
- Routing-prefix stripping and the own-endpoint raw-HTTP requirement moved from
  a hardcoded `ollama/`/`fireworks/` list in the chat loop onto the adapters
  themselves, so a provider added later inherits both instead of rediscovering
  the bug.
