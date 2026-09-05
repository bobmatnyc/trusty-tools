Added

- New public `control_bus` module carrying the shared event types every harness
  and trusty-console agree on: the `HarnessEvent` envelope, its `HarnessPayload`
  domain union, the `LifecycleEvent` / `HarnessSource` taxonomy, and the
  subscriber-side `Filter`. Moved verbatim from `trusty_agents_common::events`
  so consuming the envelope no longer means depending on a producer crate.
  Types only — no channel and no global state, enforced against the module's own
  sources by `control_bus::tests::control_bus_declares_no_transport`. Ungated:
  `serde`, `serde_json` and `chrono` were already unconditional, so the module
  adds no dependency. (#6846)
