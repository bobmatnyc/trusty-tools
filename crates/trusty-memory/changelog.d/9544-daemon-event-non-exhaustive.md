Breaking
- `DaemonEvent` is now `#[non_exhaustive]` and gains the `PalaceRenamed` variant. An exhaustive `match` on it outside trusty-memory no longer compiles; add a `_` arm. Shipped as a patch release by owner ruling (VERSIONING 2026-09-26) (#9544).
