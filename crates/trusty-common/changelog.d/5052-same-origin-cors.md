Added

- **`same_origin_cors` and `with_guarded_middleware_same_origin_cors`** — the
  standard daemon middleware stack with the permissive CORS policy swapped for
  one that reflects only same-machine origins: loopback,
  a local webview shell (`origin_is_local_webview`), or the daemon's own
  resolved non-loopback bind ([#5052](https://github.com/bobmatnyc/trusty-tools/issues/5052)).
  `with_guarded_middleware`'s write guard is method-gated, so it never covered
  cross-origin READS; a daemon whose GET surface carries sensitive content opts
  into this variant instead. trusty-agents is the first consumer; the other
  daemons keep `with_guarded_middleware` until each is reviewed on its own.
