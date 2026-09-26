Fixed
- The `review_pr` MCP tool now reviews a PR against the trusty-search index
  registered for that PR's own `owner/repo`, looked up per call from the
  `repo_identity` trusty-search records for each index (filtered by the daemon,
  `?repo_identity=`). It no longer uses the index the MCP server resolved from
  its own working directory at startup, or the `"main"` fallback. The session's
  configured index is still used when it belongs to the PR's repo; among
  several indexes of one repo the most recently used wins. When no index
  belongs to the repo, the tool returns an error naming the repo and the index
  id it looked up — and, for a fork, the session index and the repo it belongs
  to — instead of an `UNKNOWN` verdict with `infra_unavailable` (#8649).
- When the index list cannot be read (trusty-search down) and search is not
  required, `review_pr` runs a DEGRADED diff-only review with no index, no code
  context and no static analysis, as before. With
  `TRUSTY_REVIEW_REQUIRE_SEARCH=true` it returns the error instead (#8649).
- An index found only by its bare repo name is used only when it has no
  recorded `repo_identity` (with a warning); one whose identity is unreadable
  or names another repo is refused (#8649).
