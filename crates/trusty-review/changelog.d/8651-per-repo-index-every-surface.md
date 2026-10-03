Fixed

- The webhook drain, the service `review` operation and `trusty-review run` on
  a GitHub PR now review against the PR repo's own trusty-search index, as
  `review_pr` has since #8649. Before, they used the index resolved once at
  startup, or `"main"`. A repo with no index is an error that names the repo
  and the index id. An unreadable index registry is an error on these
  surfaces unless search is opted out (`TRUSTY_REVIEW_REQUIRE_SEARCH=false`),
  which runs a degraded diff-only review. A drain delivery whose index cannot
  be resolved is kept and retried. `run` still honours `TRUSTY_SEARCH_INDEX`
  and `--source-root` when either is set.
