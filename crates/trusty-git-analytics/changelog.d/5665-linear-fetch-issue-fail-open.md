Fixed

- Linear `fetch_issue` no longer reports an auth or HTTP failure as `Ok(None)`. A non-2xx response is an error carrying the status and Linear's own message, so a rejected API key is no longer indistinguishable from an absent issue (#5665).
- `fetch_referenced_issues` returns `Result<Vec<LinearIssue>>` and stops at the first failure instead of returning an empty vec. A run against an invalid-but-present key now records `Linear: fetch issues failed: …` in the collection summary rather than writing zero rows and exiting 0 with no diagnostic. This changes the public signature of `LinearClient::fetch_referenced_issues`.
