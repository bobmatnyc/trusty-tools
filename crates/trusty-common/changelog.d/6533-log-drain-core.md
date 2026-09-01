Added

- `log_drain` module (behind the new `log-drain` feature): uploads trusty-* log
  files to object storage. Ships the `LogDestination` trait with `s3://` and
  `file://` adapters over `object_store`, a closed-scheme `DestinationUri` parser
  (`gs://`/`az://` are reserved and refused), the
  `<prefix>/<github_id>/<session_id>/logs/<crate>/<file>` key layout, a
  size+mtime+SHA-256 manifest that makes a re-run skip unchanged files, and a
  collector that level-filters `tracing` output, scrubs secrets via
  `credentials::scrub_secrets`, and gzips each body before `run_once` uploads it
  (#6533). S3 credentials come from the AWS default provider chain already used
  by the Bedrock adapters; no bucket or region is hardcoded. This is the drain
  core only — no scheduler, no consumers, and no GitHub-identity resolution: the
  caller supplies a `DrainTarget`, and an empty `github_id` or `session_id` is
  refused rather than defaulted. The `log-drain` feature implies `credentials`
  so the scrub cannot be compiled out from under the collector.
