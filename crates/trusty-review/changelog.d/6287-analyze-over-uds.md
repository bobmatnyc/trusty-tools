Breaking

- `HttpAnalyzeClient` is removed. Its one remaining caller, the non-default
  `review` feature's `build_review_state`, now builds a `SubprocessAnalyzeClient`,
  which needs no daemon at all. The `AnalyzeClient` trait and its response types
  are unchanged.
- `ReviewConfig::analyzer_url` becomes `analyzer_socket: PathBuf`, and
  `DEFAULT_ANALYZER_URL` becomes `default_analyzer_socket()`. The environment
  variable is `PR_INTELLIGENCE_ANALYZER_SOCKET`, not
  `PR_INTELLIGENCE_ANALYZER_URL`.
- `report::HttpAnalyzeMetricsSource::new` takes a socket path instead of a base
  URL, and `AnalyzeAdapterError::Api { status, body }` becomes
  `Rpc { code, message }`.

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
