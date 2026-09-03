Added

- `tga install` runs without a terminal. `--host <local|github|bitbucket>` and
  `--pm <none|github|jira|linear>` select the non-interactive path, along with
  `--org`, `--workspace`, `--repo`, `--repo-path`, `--repo-cache`,
  `--host-token`, the `--jira-*` / `--linear-*` credentials, `--output-dir`,
  `--llm-provider` and `--llm-api-key`; `--non-interactive` forces it, and stdin
  not being a terminal implies it. A flag value wins over its environment
  variable (`GITHUB_TOKEN`, `BITBUCKET_TOKEN`, `JIRA_URL`, `JIRA_EMAIL`,
  `JIRA_API_TOKEN`, `LINEAR_API_KEY`), and a credential taken from the
  environment is written to the config as a `${VAR}` reference rather than in
  the clear. Run with no terminal and no flags, install now names every missing
  flag at once instead of blocking on the first prompt. (#5216)
- `tga install --host github --org <ORG>` derives the repository set from the
  GitHub API — `discover_org_repos` was already paging `GET /orgs/{org}/repos`
  for PR collection and is now what populates a generated config's
  `repositories:` list, so an operator no longer has to name paths that already
  exist locally. An org the token cannot see records that in the config instead
  of emitting a silently empty one. (#5216)
- Linear joins JIRA and GitHub Issues in the project-management choices, and
  Bitbucket Cloud joins GitHub in the host choices, in both the wizard and the
  flag path. Bitbucket takes an explicit `--repo <workspace/slug>` list and says
  workspace discovery is not available yet (#5220) rather than producing an
  empty repository set. (#5216)
