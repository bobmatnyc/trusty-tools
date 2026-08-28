Fixed
- `tctl install` and `tctl upgrade` evict the launchd unit trusty-analyze left
  loaded, through the same `RETIRED_SERVICES` mechanism that clears
  `com.trusty.review` (#6290) — one eviction path, now two rows. Without it an
  upgraded macOS host kept `com.trusty.analyze` loaded under `KeepAlive: Always`,
  which restarted the on-demand trusty-analyze server every time its idle window
  reclaimed it. A unit that will not go down fails that member's install or
  upgrade rather than being reported as a skip (#6350).
- `tctl start`/`restart`/`upgrade` no longer route trusty-analyze through
  launchd. Its retired row makes `manage_strategy_for` return
  `ManageStrategy::None`, so `start` cannot bootstrap the retired unit on an
  upgraded host or shell out to a `trusty-analyze service install` that no
  longer exists on a fresh one. The port guard could not have caught either:
  analyze has bound no port since #6287, so the guard permits vacuously (#6350).
