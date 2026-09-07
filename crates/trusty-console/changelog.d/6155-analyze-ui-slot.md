Added
- The trusty-analyze dashboard is served from the console at `/tools/analyze/`.
  Its Svelte source moved into this crate as `ui-analyze/`, alongside
  `ui-search/` and `ui-memory/`, and build.rs builds it into the committed
  `ui-analyze-dist/` bundle
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- `ANY /api/analyze/{*path}` (and the deprecated `/proxy/analyze/{*path}` alias)
  translate each path the dashboard calls into one `analyze.*` JSON-RPC call on
  trusty-analyze's Unix socket. trusty-analyze has had no HTTP surface since
  #6287, so this is the only way in
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The analyze bridge gives one exchange 120 seconds rather than the 30 the
  other two use. `analyze.clusters` pulls a whole index's chunks out of
  trusty-search and runs k-means over them; on the `trusty-tools` index that
  took 38.8 s, so a 30-second budget refused work the daemon was still doing
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
