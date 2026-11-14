Security
- The daemon log no longer records the AWS access key ID: every `tm` tracing subscriber holds the `aws*` targets (`aws_config`, `aws_sdk_*`, `aws_smithy_*`, `aws_sigv4`, `aws_credential_types`) at WARN, including under `RUST_LOG=info`. An explicit `RUST_LOG` directive such as `aws_config=debug` still opts back in for that target (#8948).
