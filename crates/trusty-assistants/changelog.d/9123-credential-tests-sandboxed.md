Fixed

- Credential tests for the Anthropic, Fireworks and AtlasCloud endpoints, the
  raw-completion missing-key path, credential selection and the "other
  configured providers" diagnostic, provider pinning, Brave search, the GitHub
  ticketing client and channel credential refs now run `#[serial]` inside the shared credential
  sandbox. None can reach the developer's `.env.local`, `$HOME` store or
  ambient tokens, and none prints a resolved key on failure. The channel, MCP
  auth and Telegram-gateway env tests moved from keyed serial groups to the
  unkeyed group, which the sandbox also holds (#9123).
