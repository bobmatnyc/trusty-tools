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
