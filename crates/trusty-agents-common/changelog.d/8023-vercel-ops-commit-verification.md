Documentation

- `vercel-ops` now names `vercel ls --prod --json` and the API's `GET
  /v6/deployments` `meta.githubCommitSha` field as the source of truth for
  which commit a deployment serves, and the git-sourced `POST
  /v13/deployments` as the redeploy path after a dropped webhook
  (refs [#8023](https://github.com/bobmatnyc/trusty-tools/issues/8023))
