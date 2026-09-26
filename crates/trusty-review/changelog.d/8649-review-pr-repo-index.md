Fixed
- The `review_pr` MCP tool now reviews a PR against the trusty-search index
  registered for that PR's own `owner/repo`, looked up per call from the
  `repo_identity` trusty-search records for each index. It no longer uses the
  index the MCP server resolved from its own working directory at startup, or
  the `"main"` fallback. The session's configured index is still used when it
  belongs to the PR's repo. When no index belongs to the repo, the tool returns
  an error naming the repo and the index id it looked up, instead of an
  `UNKNOWN` verdict with `infra_unavailable` (#8649).
