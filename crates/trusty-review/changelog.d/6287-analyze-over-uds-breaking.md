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
