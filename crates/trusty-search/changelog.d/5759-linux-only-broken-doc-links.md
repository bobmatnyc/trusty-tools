Fixed
- Stopped `memguard`'s doc comment from linking to
  `trusty_common::sys_metrics::physical_footprint_mb`, which is macOS-gated and
  so unresolvable on Linux. The link is cross-crate, which is why repairing
  trusty-common's own six links did not fix it.
