Fixed
- `GET /health` and `mpm.health` serve the catalog-staleness report from a 30-second cache that refreshes on the blocking pool. They no longer walk the deployed catalog inline on every request, so the liveness probe answers under host I/O load. The report reads `catalog_unknown` until the first walk finishes and whenever a walk fails (#7968).
