Added

- An `s3://` log-drain destination can name its own AWS identity:
  `?profile=<name>` reads credentials and region from that `~/.aws` profile
  alone, and `?role_arn=<arn>` assumes that role over whatever base identity
  resolved (#6657). Both combine, and both sit beside the existing `?region=`;
  every other query key is still rejected, and a repeated key is now rejected
  rather than resolving to the last occurrence. Without either, the destination
  keeps today's AWS default provider chain.
- `DestinationUri::cache_namespace()` now separates two identities against one
  bucket, so a skip decision recorded under one profile is never read back under
  another. A destination that pins no identity keeps the namespace it already
  had, so upgrading orphans no existing manifest cache.
- `github_path::parse_remote_url` parses a git remote URL into its host and its
  `owner/repo`, keeping the case the remote spells them in, and
  `github_path::derive_remote_repo` reads a directory's `origin` and parses it
  (#6657). This is the workspace's one git-remote-URL parser: it replaces the
  three private copies `trusty-git-analytics` (twice) and `trusty-code` carried.
  Unlike `parse_github_path` it does not slugify, carries the host so a caller
  can filter on it, and refuses a path that is not exactly `<owner>/<repo>`.
