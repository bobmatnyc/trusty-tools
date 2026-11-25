Added
- `GET`/`PUT /api/channels` read and replace the harness-wide `[[channels]]` table, with the same revision/compare-and-swap discipline as the per-assistant route. A legacy `[[listeners]]` entry shows up as the channel it is; the write preserves unrelated tables and comments in `config.toml`, though not comments written inside a `[[channels]]` table.
- The `channel` tool absorbs `listener_config`: `list`/`get`/`set` over both the assistant and global scopes, plus the existing `read` and `send`.
