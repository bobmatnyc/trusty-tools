Fixed

- `service install` reads operator tunables from the LEGACY unit when the
  canonical plist does not exist yet. On the host this issue describes — one
  whose live agent still carries the old label — the read returned nothing, so
  `TRUSTY_NO_AUTO_DISCOVER`, `TRUSTY_DEVICE` and `TRUSTY_BM25_CORPUS_CAP` were
  silently dropped moments before eviction deleted the plist holding the only
  record. That defeated #4823 precisely on the migration path this change
  introduces (#4868)
- `service install --force` reloads even when the rendered unit is unchanged,
  and `make deploy` uses it. A deploy changes the BINARY behind a byte-identical
  plist, so without it the install reported the unit already current and launchd
  kept running the old image. `deploy` also boots the job out before
  `cargo install` — the unit is `KeepAlive::Always` (#4113), so a SIGTERM'd
  daemon is relaunched into the middle of the install and gets its binary
  swapped underneath it, which is #87 (#4868)
