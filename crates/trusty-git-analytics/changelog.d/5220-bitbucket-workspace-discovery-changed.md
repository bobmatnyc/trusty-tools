Changed

- `bitbucket.repo_slug` and `bitbucket.workspace` are required only when
  `bitbucket.workspaces` is empty. A config that names workspaces to discover
  supplies neither. (#5220)
- `BitbucketClient::fetch_pr_commits` takes the workspace and repository slug as
  arguments rather than reading them off the client, which now covers many
  repositories. (#5220)
