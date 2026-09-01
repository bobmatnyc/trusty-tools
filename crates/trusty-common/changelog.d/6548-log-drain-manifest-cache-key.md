Fixed

- `log_drain`'s local manifest cache is now keyed by the destination as well as
  the identity, so switching `log_drain.destination` from one bucket to another
  re-uploads instead of silently skipping. The cache lived at
  `<state_dir>/log-drain/<github_id>/<session_id>/`, which described no
  particular destination: repointing a session at a brand-new bucket found the
  record written for the old one, and because the new bucket had no remote
  manifest to override it, every previously drained file was classified
  `SkipUnchanged` — 86 of them never arrived, while sources with no prior entry
  uploaded normally (#6548). The cache path now carries
  `DestinationUri::cache_namespace()` — scheme, bucket-or-path and key prefix,
  hashed; `?region=` is excluded so a region override does not orphan a valid
  cache. `LogDestination` gained a `cache_namespace` method, so the identity
  comes from the destination rather than from a caller who could name the wrong
  one.
- A run whose manifest came from the destination now `head`s one sampled entry
  and warns when that object is missing, reporting it in `DrainReport`'s new
  `manifest_spot_check_missing` counter. Manifests written before the keying fix
  list objects their bucket never received, and every file such a manifest names
  skips forever; the repair is to delete the manifest object, documented in
  `docs/reference/log-drain.md`.
