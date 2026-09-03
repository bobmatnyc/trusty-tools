Added
- Each registered service gets a per-second sample — `{ id, status, cpu_pct }` —
  recorded into a bounded 600-point in-memory ring, fanned out on
  `/api/console/machine-status/stream` as a new `services` event, and carried in
  the `history` snapshot under `service_samples`. `cpu_pct` is `null`, never
  `0.0`, whenever no measurement was taken: an idle bar and an unmeasurable one
  must look different on the card
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `GET /api/console/services` entries carry `cpu_pct`, so the list renders a CPU
  figure before the stream connects. It is read from the same rings the graph
  draws, so the number and the newest bar cannot disagree
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- A service's process is identified from the discovery artifact it already
  publishes: the `pid` line in trusty-mpm's `daemon.lock`, and the peer pid of
  the Unix socket for trusty-search and trusty-memory. trusty-agents serves TCP
  loopback, which carries no pid, and trusty-analyze and trusty-review are
  on-demand members with no resident process — those three report `null`. A
  lookup that fails backs off for 15 s rather than retrying every tick
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
