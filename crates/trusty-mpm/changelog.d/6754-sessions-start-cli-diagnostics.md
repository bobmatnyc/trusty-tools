Fixed

- `tm sessions start` now registers the CLI diagnostics subscriber, so the in-place deploy path's stray-skill sweep reports its removals and failed removals on stderr instead of dropping them (#6754).
