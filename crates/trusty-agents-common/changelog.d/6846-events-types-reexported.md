Changed

- `events` now re-exports `HarnessEvent`, `HarnessPayload`, `LifecycleEvent`,
  `HarnessSource` and `Filter` from the new `trusty_common::control_bus` instead
  of declaring them. Every name resolves at the same `events::*` path with the
  same shape, so no consumer changes. The process-global broadcast channel, the
  `__OMPM_EVENT__` stderr relay, `Lag`, `BusClosed` and `CHANNEL_CAPACITY` stay
  here — the transport is not the type. (#6846)
