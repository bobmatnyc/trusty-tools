Added
- `LoadReport` has a new public `fingerprint: u64` field. A report from a `PolicyLoader` whose load settled carries a nonzero digest of the sources it was loaded from, and the digest changes whenever the loader reloads. A load that did not settle (#9808), a one-shot load and the placeholder report carry 0, which means a cache must never key on that report (#8454).
