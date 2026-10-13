Fixed
- The daemon now listens for an in-process stop before it binds its socket. A stop sent after the socket accepted a connection but before the daemon subscribed was dropped, and the daemon kept serving; `a_normal_stop_leaves_every_corpus_openable_read_only` timed out at 20 s on a loaded CI shard for this reason (#9736).
