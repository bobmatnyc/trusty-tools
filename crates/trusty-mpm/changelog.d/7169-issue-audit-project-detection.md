Fixed

- `tm issue standard` no longer reports the projects section "unavailable"
  when a token lacks `read:project` scope for the owner-qualified
  `gh project list --owner <login>` query but the same token's bare
  `gh project list` (the viewer's own projects) succeeds — it now falls back
  to the bare call on a scope-shaped failure before giving up (#7169).
