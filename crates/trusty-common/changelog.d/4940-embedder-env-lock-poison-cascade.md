Fixed

- one failing embedder test no longer cascades into seven. `ENV_LOCK.lock().unwrap()` poisoned the shared `Mutex<()>` when the ONNX accuracy gate panicked, so the six `resolve_*` tests — pure model/provider/cache-dir resolution that never touches fastembed — failed with `PoisonError` and buried the real cause. Call sites now go through `test_env::env_lock()`, which recovers via `PoisonError::into_inner`; the lock guards no data, and `EnvVarGuard::drop` restores the environment during the panicking test's unwind (closes [#4940](https://github.com/bobmatnyc/trusty-tools/issues/4940))
