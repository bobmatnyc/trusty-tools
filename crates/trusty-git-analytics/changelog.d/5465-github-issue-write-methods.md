Added

- `GitHubClient` gains issue WRITE methods (part of #5465, epic #5468): `search_issues`, `create_issue`, `create_issue_comment`, and `upsert_issue_thread`, which composes the three into find-or-create. They live in a new `collect::github::issue_writer` module rather than in `client.rs`, which was already at 289 of its 500 SLOC — this repo splits at the PR that grows the file, not in a follow-up. Every method is an inherent `impl GitHubClient`, so callers still see one client.

  `upsert_issue_thread` finds a contributor's existing thread by a marker embedded in the title and comments on it, opening a new issue only when there is none. A failed search aborts instead of falling through to create: a transient 5xx read as "no thread exists" would open a duplicate issue on every hiccup, which needs manual cleanup to undo.

  Writes reuse the client's existing `Authorization: Bearer <token>` header — the same personal access token the read path already sends. Nothing in the write path inspects the credential, so moving it onto a GitHub App installation token is a change to how the client is built.

- `GitHubClient::with_api_base` points a client at a REST root other than `https://api.github.com`, which is what lets the write tests run against a local `wiremock` server instead of github.com. The read methods take the same root, so a GitHub Enterprise host would work through one seam rather than two.
