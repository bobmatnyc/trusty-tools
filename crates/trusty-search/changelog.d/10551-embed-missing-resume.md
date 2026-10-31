Added

- `trusty-search index embed-missing`, the `search.index.embed_missing` socket method and the `embed_missing` MCP tool embed only the chunks that have no vector and settle the semantic stage, so an index left `degraded` by an embedder fault no longer needs a full reindex. `dry_run` reports the gap; a repeat run with nothing missing is a no-op; a dead embedder is refused before the stage is touched (#10551).
- The daemon retries a `Failed` semantic stage on its own: a ticker probes the embedder with a 30 s to 15 min doubling backoff and resumes the stage once it answers, giving up after 5 resumes that fail again (#10551).
