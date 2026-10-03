Fixed
- `code-critic` runs a consumer suite in the foreground with an explicit timeout and never backgrounds it to wait (#8320).
- `version-control`'s no-`tm` squash fallback passes the live PR title plus ` (#N)` as the subject and the live PR body as the message; a brief never overrides them (#8420).
- `vercel-ops` warns that a bare `vercel link` can create a stray project, and that `vercel env add --force` can leave a stale duplicate (#8321).
- `local-ops` checks the operator's egress IP against the allowlist before allowlist-dependent work (#8470).
