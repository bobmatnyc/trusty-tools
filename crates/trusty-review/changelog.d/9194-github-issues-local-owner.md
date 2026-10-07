Fixed
- The `github_issues` context source no longer queries the GitHub Search API for a local diff, which has no repository; the query answered 422 and contributed nothing (#9194).
