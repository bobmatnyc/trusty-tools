Added

- Bitbucket Cloud workspace-to-repository discovery. `bitbucket.workspaces` is a
  new config list whose every entry is paged over
  `GET /2.0/repositories/{workspace}` — Bitbucket's `next`-cursor convention,
  the same shape `discover_org_repos` has had for a GitHub org since #742. The
  discovered repositories are unioned with the singular
  `bitbucket.workspace`/`repo_slug` pair, and one `BitbucketClient` now collects
  pull requests across the whole set instead of one repository. A workspace that
  fails is logged and skipped, and a repository that fails no longer discards
  the rest of the batch. (#5220)
- `tga install --host bitbucket --workspace <WORKSPACE>` derives the repository
  set from the API, so `--repo` is now optional there. The generated config
  emits `bitbucket.workspaces`, so `tga collect --validate-only` accepts it. The
  generated block used to name a `workspace` with `fetch_prs: true` and no
  `repo_slug` — it deserialized, but validation then refused it with "Bitbucket
  config incomplete: repo_slug is required when fetch_prs = true". (#5220)
