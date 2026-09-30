Security

- `trusty-review` no longer prints the AWS access key ID on stderr. The AWS
  credential provider logs it at INFO, and a `RUST_LOG=info` meant for
  trusty-review's own events (the value agent sessions export) let it through.
  The stderr filter now holds `aws_config` and `aws_sdk*` at `warn` unless
  `RUST_LOG` names one of those targets itself. An unparsable `RUST_LOG` falls
  back to `warn` with both guards.
