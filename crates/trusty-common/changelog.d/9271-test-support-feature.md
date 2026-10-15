Added
- New test-only `test-support` feature exposes `log_buffer::capture_logs`, `atomic_file::test_support::ReadOnlyDir` and, with `credentials`, `credentials::env_guard::EnvVarGuard` to another crate's tests; the engine's tests in trusty-memory use them. It implies nothing and belongs in `[dev-dependencies]` only ([#9271](https://github.com/bobmatnyc/trusty-tools/issues/9271))
