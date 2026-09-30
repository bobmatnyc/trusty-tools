Security

- `trusty-review` no longer prints AWS credential material on stderr. The AWS
  credential provider logs the access key ID at INFO, and the SigV4 signer
  logs the key ID and the session token at TRACE; a `RUST_LOG=info` or
  `RUST_LOG=trace` meant for trusty-review's own events (agent sessions export
  `RUST_LOG=info`) let them through. The stderr filter now holds every `aws*`
  target at `warn` unless `RUST_LOG` has a directive for exactly `aws`. A
  directive for one AWS crate or module, such as `aws_config::imds=debug`,
  opens only that target. An unparsable `RUST_LOG` falls back to `warn` with
  the guard.
