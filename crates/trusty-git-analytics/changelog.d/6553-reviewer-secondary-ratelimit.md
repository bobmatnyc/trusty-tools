Fixed

- `tga collect`: a GitHub secondary rate limit during the reviewer pass no longer
  fails the run (#6553). The pass now tells a throttled pull request apart from a
  broken one, records the shortfall once — naming how many pull requests got no
  reviewer rows — and records it at `ItemSkipped` severity, so `tga collect`
  exits 0 with `pr_reviewers` partial rather than exiting 1 with every other
  stage's data already persisted. The reviewer query is forward-only, so the next
  run resumes at the pull requests the throttled one never reached. This replaces
  the #6084 abort, which made `github.fetch_pr_reviews: true` unusable on an
  unattended schedule and emitted one warning line per remaining pull request
  (21,230 in the reported run).
