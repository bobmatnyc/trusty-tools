Added

- `memory_connector_accepts_the_envelope_a_real_daemon_sends` replays a
  `memory.health` frame captured verbatim from a live trusty-memory 0.25.2 over
  its socket. The existing tests reply with only the two fields the connector
  deserialises, so none of them would catch the connector refusing the eight
  extra fields a real daemon sends — the shape of the #6356 recurrence (#6356)
