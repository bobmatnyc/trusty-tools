Fixed

- `--analyze` no longer degrades to a scan-only report when the analyze daemon
  closes a pooled connection (#6038). The adapter issues its GETs back to back
  on one HTTP/1.1 keep-alive connection, and the daemon closing that connection
  races the next request — the diagnostics fetch that followed a slow
  `/complexity_distribution` failed with `error sending request`, and every
  metrics-driven section fell back to the scan. A transport failure that is
  neither a timeout nor a refused connect is now retried once on a fresh
  connection; a refused connect and a timeout stay terminal, so a daemon that is
  genuinely down still fails open at the same speed.
- The default analyzer URL is `http://127.0.0.1:7879` rather than
  `http://localhost:7879` (#6038). `trusty-analyze serve` binds the IPv4
  loopback only, and macOS resolves `localhost` to `::1` first, so a stock run
  could not reach a healthy daemon until the operator exported
  `PR_INTELLIGENCE_ANALYZER_URL` by hand. The env-var override is unchanged.
- Both analyze HTTP clients now build through
  `trusty_common::http_client::loopback_client_builder`, so an exported
  `HTTP_PROXY` no longer diverts a loopback fetch to a proxy (#4392).
