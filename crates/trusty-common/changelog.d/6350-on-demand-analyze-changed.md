Changed
- `launchd_labels` splits its registry in two: `SERVICES` is what an install
  writes, `RETIRED_SERVICES` is what an upgrade must clear. `com.trusty.analyze`
  moves to the second table — named so a migration can still evict a unit an
  older installer left loaded, separate so nothing installs it again.
  `retired_labels_for_member` returns that eviction set, canonical label first
  (#6350).
