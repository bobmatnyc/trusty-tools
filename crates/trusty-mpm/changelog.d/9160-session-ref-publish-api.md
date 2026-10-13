Changed
- `core::session_ref_publish::SessionRefRequest` gains a `git_env` field, and `remote_tip` and `push_session_ref` take an `env` argument: the env the session-ref network calls run under. Pass `&[]` for the previous ambient-credential behaviour (#9160).
