Added

- **`GET /health` reports `resident_index_cap` and `resident_index_cap_source`.** The cap is the acting number (`null` when it is off); the source is `"env"`, `"env (off)"`, or `"tier default"`. The same pair is logged once at startup. Without them the number that decides whether an index gets parked was only readable from the daemon's environment ([#6821](https://github.com/bobmatnyc/trusty-tools/issues/6821))
- **`SearchAppState::machine_tier`.** The residency sweep and `/health` both need the machine tier, and reading total RAM spawns `sysctl` on macOS — resolving it once per state keeps a process spawn off a per-poll endpoint ([#6821](https://github.com/bobmatnyc/trusty-tools/issues/6821))
