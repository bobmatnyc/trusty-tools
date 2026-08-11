Fixed

- A relocated `tm launch` / `tm connect` now carries `CLAUDE_CODE_OAUTH_TOKEN`
  when one is stored. On macOS the Keychain entry Claude Code reads is keyed by
  a hash of `CLAUDE_CONFIG_DIR`, so relocating without the token produces a
  login that succeeds and is then immediately not-logged-in. When no token
  resolves, the commands print a one-line notice naming
  `claude setup-token | tm auth set-token` rather than leaving the operator to
  diagnose the loop (#2246, #4181).
