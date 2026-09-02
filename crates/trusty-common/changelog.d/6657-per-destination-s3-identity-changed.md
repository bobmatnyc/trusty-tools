Changed

- A pinned `?profile=` is the WHOLE identity for that destination: the default
  chain is not consulted, so `AWS_ACCESS_KEY_ID` in a daemon's environment can
  no longer silently re-point one destination at another account. Region
  resolution for such a destination is `?region=` → the profile's own `region` →
  refusal with `DrainError::Credentials`.
- The log-drain key layout is now `<owner>/<project>/<crate>/<file>` beneath the
  destination prefix, replacing `<github_id>/<session_id>/logs/…` (#6657).
  `DrainTarget` carries `owner` and `project` instead, and `logs_prefix()` is
  now `key_prefix()`. Objects already uploaded under the old layout stay where
  they are — the manifest is keyed by destination and key, so the first pass
  after the upgrade re-uploads the current files under the new prefix and then
  dedupes as before.
