Fixed

- `tm doctor`'s `daemon_version` check compares a build fingerprint, not semver alone, so a daemon started before a same-version merge is reported stale instead of matching (refs [#7822](https://github.com/bobmatnyc/trusty-tools/issues/7822))
  - `/health` publishes `build_id`, the fingerprint of the executable the daemon started from; a daemon that omits it reports "cannot be determined" rather than OK
