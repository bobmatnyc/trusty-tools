Fixed
- The `tm-capabilities` doctor reference describes the `events` row as it now behaves: it warns on a host without the trusty-events bus, and its not-running remedy is `tctl start trusty-events` only, not `tctl install trusty-events` (#10151).
