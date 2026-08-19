Fixed

- `tga audit` defaults the analyze daemon address to `http://127.0.0.1:7879`
  rather than `http://localhost:7879` (#6038). It must match
  `trusty-review`'s default, which moved for the same reason: `trusty-analyze
  serve` binds the IPv4 loopback only, and macOS resolves `localhost` to `::1`
  first. `PR_INTELLIGENCE_ANALYZER_URL` still overrides it.
