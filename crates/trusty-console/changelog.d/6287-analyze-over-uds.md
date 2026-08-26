Changed

- `AnalyzeConnector` dials trusty-analyze's Unix socket instead of probing
  `127.0.0.1:7879` (#6287, ADR-0032), and reports `url: None` — a UDS daemon has
  no URL, so a synthesised `http://` address would be a link that cannot work.
  The pre-migration fallback to port 7879 is gone: any process holding that port
  used to make the dashboard report a trusty-analyze that was not there.
- The `analyze` row is removed from the reverse-proxy allowlist, and 7879 from
  the `known_siblings` port-collision guard — a guard naming a port nothing binds
  refuses a value that is free.
