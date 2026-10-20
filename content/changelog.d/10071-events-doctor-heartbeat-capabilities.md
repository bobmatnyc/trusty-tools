Changed
- The `tm-capabilities` doctor reference describes the `events` row's heartbeat probe: `ok` only when the bus answers a subscription, `fail` when it is hung, and `warn` when it is too old to accept `heartbeat_ms` (#10071). A stale socket now names both causes, an exited daemon or a hung one with a full backlog, and the remedy `tctl restart trusty-events`.
