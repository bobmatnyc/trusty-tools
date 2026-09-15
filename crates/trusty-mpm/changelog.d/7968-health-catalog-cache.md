Fixed
- `GET /health` and `mpm.health` serve the catalog-staleness report from a 30-second cache that refreshes on the blocking pool. They no longer walk the deployed catalog inline on every request, so the liveness probe answers under host I/O load. The report reads `catalog_unknown` until the first walk finishes and whenever a walk fails (#7968).
- A refresh outstanding longer than 60 seconds is now presumed hung: `GET /health` and `mpm.health` report `catalog_unknown` instead of re-serving the last good report indefinitely (#7968).
