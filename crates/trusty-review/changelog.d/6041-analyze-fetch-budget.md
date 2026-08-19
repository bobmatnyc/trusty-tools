Fixed

- `--analyze` gives each trusty-analyze endpoint its own request budget instead
  of one flat 15 s. The diagnostics budget is derived from the daemon's own
  deadline ladder (`TRUSTY_DIAGNOSTICS_DEADLINE_SECS`, default 180 s, plus its
  90 s of handler/router/client headroom), so the daemon's answer — including
  the structured report it sends when its own deadline is hit — always outlasts
  the client. Measured live before the fix: diagnostics answered `200` at 142 s
  with `tools_run: ["clippy"]` while the client gave up at 15 s. The cheap
  endpoints keep the short budget. See #6041.
- A trusty-analyze endpoint that fails no longer discards the data the other
  endpoints returned. Complexity, diagnostics, and refactor suggestions are
  fetched independently; whatever answered is rendered, and each endpoint that
  did not is named under Gaps & Caveats along with why it dropped out (timed
  out, unreachable, or an unusable answer). A fetch where no endpoint answered
  still falls back to the repository scan rather than rendering empty metrics.
  See #6041.
