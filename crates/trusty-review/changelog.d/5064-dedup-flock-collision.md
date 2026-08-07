Fixed

- `dedup.redb` no longer locks out sibling processes. The dedup claim store held
  redb's exclusive file lock for the whole process lifetime, so a second opener
  against the same `--log-dir` — `serve --stdio` alongside `serve`, or an
  ADR-0034 console-spawned webhook worker alongside the HTTP daemon — got
  `DatabaseAlreadyOpen`, which `build_app_state` downgraded to a warning and ran
  on with no dedup at all. The store now opens redb per operation and releases it
  again, waiting out a concurrent holder for up to 2s and returning a typed
  `DedupError::Contended` if it never frees. `serve --stdio` no longer opens the
  file at all (its MCP tools are `allow_posting: false`), and a server mode that
  can post now fails to start rather than starting without the claim gate
  (#5064).
