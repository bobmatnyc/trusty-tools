Fixed
- `--source-root` no longer overrides an explicit `TRUSTY_REVIEW_REQUIRE_SEARCH=true` (or `[context] require_search = true`) when trusty-search is unreachable or has no index for the directory; the review now ends Skipped instead of returning a context-free verdict (#10226).
