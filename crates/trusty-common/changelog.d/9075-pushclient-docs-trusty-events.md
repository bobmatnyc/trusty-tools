Changed
- `control_bus::PushClient` and the `control_bus` module docs now name the trusty-events socket (`events_socket_path()`) as the one producers push to, since the bus moved from trusty-console to the `trusty-events` daemon (ADR-0065). Documentation only; no API or behaviour change (#9075).
