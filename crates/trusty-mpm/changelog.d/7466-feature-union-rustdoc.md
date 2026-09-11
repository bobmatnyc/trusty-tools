Fixed

- The crate now compiles and documents cleanly under its full feature union. The
  `sm-memory` and `manager-memory` call sites read `Drawer::content` through its
  `content()` accessor (the field became private in #5902 and no default build
  compiles those modules), and the five intra-doc links to `ProviderRegistry`,
  `PalaceRegistry` and `PalaceId` resolve by explicit path, so the per-lane
  rustdoc gate is green for `trusty-mpm` (#7466).
