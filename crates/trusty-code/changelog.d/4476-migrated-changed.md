Changed

- **`events_connect_hang_is_bounded_by_connect_timeout_not_infinite` waits out
  `CONNECT_TIMEOUT` on Tokio's virtual clock** — 10.01s to 0.01s. The pause is
  taken only AFTER the listener confirms it accepted the pump's connection, so
  the #3494 "TCP accepted, headers never sent" precondition is established for
  real before time is faked. A blanket `#[tokio::test(start_paused = true)]`
  here is NOT equivalent and was rejected on measurement: it lost the accept on
  6 of 10 runs (Tokio auto-advances while the handshake is still in flight),
  quietly downgrading this to a plain connect-timeout test while still passing,
  because both failure modes produce a "connection failed" reason. With the
  ordering above the emitted reason string is byte-identical to the real-clock
  run, and removing `.timeout(CONNECT_TIMEOUT)` from `pump_session_events`
  still fails the test.
