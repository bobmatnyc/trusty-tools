Fixed
- `search.health` reports `embedderd_rss_mb` for a lazily spawned embed sidecar (the `TRUSTY_EMBEDDER=python` arm) and follows it across a recycle, a crash restart and an idle shutdown. The daemon's pid forwarder used to stop at the first `0` pid, which a lazy sidecar reads before its first spawn; it now treats `0` as "no sidecar for now" and stops at daemon shutdown (#9828).
