Fixed

- The daemon no longer logs a remote URL's embedded credentials. The managed
  base clone logged `remote.origin.url` verbatim, so an origin of
  `https://<user>:<token>@github.com/…` wrote the token to `trusty-mpm.log`.
  One helper, `core::remote_url_redact::redact_url`, now replaces the userinfo
  with `***` at the clone log line, the clone's git stderr, the unparseable
  remote warning, the cold-start remote-mismatch errors, the gh-account spawn
  warnings, the registry and catalog-sync log lines, the standalone clone
  error, and `tm`'s clone and fallback messages (#9124).
