Added

- `tm doctor --network` adds a `gcloud_auth` row. It names the active
  gcloud account, redacted to `a***@domain`, and says whether `gcloud` can
  mint an access token for it. It tells "no credentials" apart from
  "credentials present, but every account needs an interactive reauth", and
  names `! gcloud auth login` as the operator step for both. Each `gcloud`
  call is bounded at 10 s. A missing binary, a timeout, a non-zero exit or
  unparseable output is a WARN naming the reason, never a pass. No token
  value is printed. A bare `tm doctor` stays offline: the row reads
  `skipped (needs --network)` and `gcloud` is never spawned.
