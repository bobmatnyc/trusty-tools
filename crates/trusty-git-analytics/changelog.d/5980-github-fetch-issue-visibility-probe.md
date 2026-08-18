Fixed

- `GitHubClient::fetch_issue` no longer treats a private repository invisible
  to the configured credential as "issue not found". GitHub answers every
  request under such a repo with 404, so a missing token or one that cannot
  see the repo used to look identical to a genuinely deleted issue and the
  caller silently collected nothing. A repo-visibility probe now
  distinguishes the two, cached once per client so it does not repeat on
  every subsequent lookup, and `fetch_issue` retries on 429/5xx the same as
  `list_issues` and `fetch_pr_commits` (#5980).
