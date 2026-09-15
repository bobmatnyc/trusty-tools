Fixed

- `trusty-search index <root> --force` on a root already registered under another index id now reindexes that index instead of failing with `daemon returned 409 Conflict for POST /indexes`, which contradicted the flag's own `--help` ([#7758](https://github.com/bobmatnyc/trusty-tools/issues/7758)). Without `--force` the refusal is unchanged.
- The daemon's Unix socket path now honours `TRUSTY_DATA_DIR` / `--data-dir`, so a second instance no longer tries to bind the production daemon's socket and refuses to start ([#7801](https://github.com/bobmatnyc/trusty-tools/issues/7801)). `TRUSTY_DATA_DIR` is the authoritative override for isolating an instance end to end; a client reaches an isolated daemon by exporting the same value or by setting `TRUSTY_SEARCH_SOCKET`.
- `trusty-search index --no-kg` with no other filter set no longer drops `skip_kg` on the wire — the empty-filter fast path substituted default filters ([#313](https://github.com/bobmatnyc/trusty-tools/issues/313)).
