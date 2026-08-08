Added

- `bm25_client::Bm25Client::stats` and `Bm25Stats` — a caller can now ask a BM25
  daemon how much corpus it is serving. Without it an empty search result was
  ambiguous between "the query matched nothing" and "nothing is indexed", so a
  partially-backfilled palace served partial content while looking healthy.
- `sys_metrics::process_rss_mb` — resident memory of an arbitrary pid (macOS
  `phys_footprint`, Linux `/proc/<pid>/status` `VmRSS`). The one entry point
  every trusty-* supervisor uses to enforce a child-memory limit, so #2846's
  declared-but-never-compared `rss_limit_mb` cannot recur per-crate. `None`
  means "cannot measure", never "measured zero".
