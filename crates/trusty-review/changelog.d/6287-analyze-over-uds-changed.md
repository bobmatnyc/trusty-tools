Changed

- `report --analyze` dials trusty-analyze's Unix socket (#6287, ADR-0032). Its
  per-endpoint budgets, fail-open contract, and Gaps & Caveats vocabulary are
  unchanged: `classify_failure` reads JSON-RPC code `-32005` where it read HTTP
  504, so a daemon-side deadline still prints "did not answer within the time
  allowed" rather than "could not be reached".
- The #6038 keep-alive retry is removed. It recovered a pooled HTTP/1.1
  connection the daemon closed mid-request; the socket client dials per call, so
  there is no pooled connection to lose.
- The context gate's skip message no longer names an analyze daemon address —
  no path on that gate contacts one.
