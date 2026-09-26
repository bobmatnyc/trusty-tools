Added

- `PATCH /indexes/:id/config` with `vector: true` now runs the C2 embed catch-up when the vector lane is already enabled but the semantic stage is `Pending`/`Failed` — the embed-only trigger for a corpus registered with unembedded chunks, with no full reindex. It stays a no-op once the semantic stage is `Ready` or `InProgress`, and the response's `components.catch_up_started` says which happened ([#8148](https://github.com/bobmatnyc/trusty-tools/issues/8148))
