Changed

- `trusty-search index relocate` approves the destination before calling the daemon, and withdraws that approval if the call fails. Approving afterwards meant the now-gated `PATCH /indexes/:id` refused every destination that was not already approved — the normal case for a moved repo (#767).
- `SearchAppState` is `#[non_exhaustive]`. It gains fields regularly and each one was a breaking change purely because an external struct literal could name them all; taken alongside the `allowlist_paths` break so the next field costs nothing (#767).
