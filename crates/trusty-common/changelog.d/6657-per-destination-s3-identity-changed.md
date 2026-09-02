Changed

- A pinned `?profile=` is the WHOLE identity for that destination: the default
  chain is not consulted, so `AWS_ACCESS_KEY_ID` in a daemon's environment can
  no longer silently re-point one destination at another account. Region
  resolution for such a destination is `?region=` → the profile's own `region` →
  refusal with `DrainError::Credentials`.
