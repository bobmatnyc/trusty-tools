Changed
- MCP `review_pr` on a repo with no trusty-search index now returns a DEGRADED diff-only review (banner, `degraded_reason`, search `unavailable` in `context_sources`) instead of an error, unless `TRUSTY_REVIEW_REQUIRE_SEARCH=true`. The webhook and CLI GitHub-PR runs still fail closed (#11409).
