Fixed

- Linear collection now writes the source-agnostic `work_items` corpus, not only its own `linear_issues` table (#5219). Each fetched issue becomes one `work_items` row under `source = 'linear'`, keyed on the Linear identifier — the same string `LinearClient::extract_issue_ids` pulls out of a commit message, so the two sides match — and every commit that mentions a fetched identifier gets a `commit_work_items` link. Before this, only Azure DevOps wrote `work_items` in production, and every `tga.db` on the owner's machine had `work_items = 0`, so commit-to-board correlation had nothing to correlate against for Linear.

  `linear_issues` is unchanged and still written. It carries Linear-specific columns (`team_key`, `priority`, `assignee`) that `work_items` has no place for, and retiring it would drop those with no consumer asking for it; the two are written in the same pass, so they cannot drift.

  The commit query moved from `SELECT message` to `SELECT sha, message`. The SHA is what the join table needs, and reading messages alone made a per-commit link impossible even once the issues were known.

  `work_items.project` is left null. Linear's project lives behind a GraphQL field this crate does not query, and DOC-70 §6 routes project resolution through trusty-common's client instead — writing the team name into that column would fill the field DOC-70 filters on with something that is not a project.

  A failed `work_items` write is recorded in the run's error list and returned as an error from the writer, never downgraded to a zero count. Both tables are written inside one transaction, so a failure leaves neither half-written.
