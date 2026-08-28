Added

- Sixteen more JSON-RPC methods on the daemon's Unix socket: the legacy session
  registry (`mpm.sessions.list` / `.register` / `.connect` / `.discover` /
  `.reap` / `.get` / `.delete` / `.pause` / `.resume` / `.command` / `.output` /
  `.pane` / `.set_pid` / `.events_poll`), the hook relay (`mpm.hooks.ingest`),
  and the polled event feed (`mpm.events.poll`). The HTTP routes they mirror are
  unchanged and still served; the socket is an additional way to reach the same
  body. The SSE streams `/events` and `/sessions/{id}/events` stay HTTP-only for
  now ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
