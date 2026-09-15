Changed
- `GET`/`PUT /api/agents/{name}/listeners` and the `listener_config` tool are deprecated. Both stay callable for one release, forward to the channel handlers and the `channel` tool, and warn once per process; the routes also answer with `Deprecation: true` and a `Link` naming their successor. Slice 7 removes them.
- Global channel validation now bounds `ingest_filter.label_ids` by the same 32 x 256 non-control rule every other filter list uses, and refuses a transport this build's poller does not implement.
