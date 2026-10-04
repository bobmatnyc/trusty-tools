Fixed

- The daemon no longer logs a remote URL's embedded credentials. The managed
  base clone logged `remote.origin.url` verbatim, so an origin of
  `https://<user>:<token>@github.com/…` wrote the token to `trusty-mpm.log`.
  One helper, `core::remote_url_redact::redact_url`, now replaces the userinfo
  with `***` — including a password holding a raw `/`, `?` or `#` — and masks
  the value of
  `access_token`, `private_token`, `oauth_token` and `token` query keys. It
  runs at the clone log line, the clone's git stderr, the start-point fetch
  error, the unparseable remote warning, the cold-start remote-mismatch
  errors, the local-spawn refusals, the gh-account spawn warnings and
  account-pin refusal, the reclaim and PR-cleanup origin refusals, both
  project-registry auto-registration log lines, the catalog-sync log lines,
  the standalone clone error, the non-local `repo_url` refusal, the
  unregistered-project 404, and `tm`'s clone, fallback, launch,
  account-preflight and `--account` messages (#9124).
