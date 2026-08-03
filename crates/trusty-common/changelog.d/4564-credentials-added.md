Added

- 13 credential environment variables that a census of production source found
  in use but that the registry could not name, so no consumer could route them
  through the env → `.env.local` → store precedence even if it wanted to:
  `GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_APP_PRIVATE_KEY`, `GITHUB_WEBHOOK_SECRET`,
  `JIRA_TOKEN`, `JIRA_API_TOKEN`, `LINEAR_API_KEY`, `BITBUCKET_TOKEN`,
  `BITBUCKET_APP_PASSWORD`, `BRAVE_API_KEY`, `GOOGLE_OAUTH_CLIENT_SECRET`,
  `SLACK_APP_TOKEN`, `TAGENT_API_TOKEN`. `SLACK_APP_TOKEN` was the sharpest gap
  — it sat unmapped between its two mapped siblings `SLACK_BOT_TOKEN` and
  `SLACK_USER_TOKEN`. The registry now covers all 23 censused names, pinned by
  `registry_covers_the_full_census`, which fails in both directions so neither
  the table nor the census can drift alone. Registering a name grants nothing
  and migrates no call site: authorization is
  [#4566](https://github.com/bobmatnyc/trusty-tools/issues/4566) and the
  consumer migration is
  [#4571](https://github.com/bobmatnyc/trusty-tools/issues/4571). Three of the
  new entries (`JIRA_TOKEN`, `JIRA_API_TOKEN`, `LINEAR_API_KEY`) are what makes
  [#4478](https://github.com/bobmatnyc/trusty-tools/issues/4478) question (b)
  answerable.
