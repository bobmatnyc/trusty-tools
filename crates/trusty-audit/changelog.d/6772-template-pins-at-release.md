Fixed

- The engagement template's `[tools]` pins now name the versions this release
  train ships. `scripts/refresh-engagement-pins.sh` sets each pin to its crate's
  current workspace version and `--check` reports the stale ones;
  `scripts/preflight-publish.sh` CHECK 10 runs that check when publishing
  trusty-audit and fails the release when a pin lags a sibling whose workspace
  version is not yet on crates.io, so the copy compiled into
  `instructions::ENGAGEMENT_TEMPLATE` and written out by `taudit distribute`
  can no longer ship a version behind the one that just published (#6772).
