Changed

- `events` now re-exports `HarnessEvent`, `HarnessPayload`, `LifecycleEvent`,
  `HarnessSource` and `Filter` from the new `trusty_common::control_bus` instead
  of declaring them. Every name resolves at the same `events::*` path with the
  same shape, so no source change is needed in a consumer. The process-global
  broadcast channel, the `__OMPM_EVENT__` stderr relay, `Lag`, `BusClosed` and
  `CHANNEL_CAPACITY` stay here — the transport is not the type. Shipped as
  `0.7.0`: `cargo-semver-checks` reports the five as removed, because a
  cross-crate re-export leaves no item definition in this crate's rustdoc, and
  the crate now requires `trusty-common ^0.49`. (#6846)
