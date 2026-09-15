Changed
- `GET`/`PUT /api/agents/{name}/listeners` and the `listener_config` tool are deprecated. Both stay callable for one release, forward to the channel handlers and the `channel` tool, and warn once per process; the routes also answer with `Deprecation: true` and a `Link` naming their successor. Slice 7 removes them.
