Changed

- `tga audit`'s analyze guard dials trusty-analyze's Unix socket instead of
  port 7879 (#6287, ADR-0032), and spawns a bare `serve` — the daemon derives
  its socket path, so passing one would start a daemon the renderer does not
  dial.
- The health verdict is now explicit. Under HTTP a degraded daemon answered 503
  and failed the probe for free; a JSON-RPC health call answers with a result
  frame either way, so the guard reads `status` and accepts only `"ok"`. That
  keeps the audit's trusty-search dependency hard on every run rather than only
  on a fresh spawn.
- `AnalyzeGuard::from_env` can now fail: a derived socket path can fail to
  resolve where a string literal could not, and guessing one would send the
  audit at a socket the daemon never binds.
