Fixed
- `run_daemon` now subscribes to its stop channel before it binds the socket, so an in-process stop sent as soon as the socket accepts is never dropped. Only in-process callers such as the tests could reach that window; `a_normal_stop_leaves_every_corpus_openable_read_only` timed out at 20 s on a loaded CI shard for this reason (#9736).
