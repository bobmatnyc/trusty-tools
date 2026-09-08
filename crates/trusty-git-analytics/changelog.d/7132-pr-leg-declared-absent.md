Fixed

- `tga audit`'s report now names the "GitHub pull requests" collection leg as
  not attempted when `github.fetch_prs` is off (or no `github:` block exists)
  against repositories whose `origin` remote is GitHub-hosted (#7132). Before
  this, a config that never enabled `github.fetch_prs` left `pull_requests`
  empty with the `collect` stage reporting `Succeeded` and nothing in the
  Gaps & Caveats section explaining why — indistinguishable, on the page, from
  an org with no PR history at all.
