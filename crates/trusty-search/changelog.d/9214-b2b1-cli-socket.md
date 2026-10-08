Changed
- `trusty-search config get|set`, `cleanup`, `convert` and the index phase of `migrate` now reach the daemon over its Unix socket instead of HTTP, so they work against a `--no-http` daemon (#9214). `config` and `cleanup` do not start the daemon; with none running they exit 1 with an error naming the socket. `convert` and `migrate` still start it.
- `cleanup` deletes nothing and exits 1 when the daemon's index list has no readable `indexes` array or any index's status read fails or is refused (#9214). It used to read an unparseable list as "no indexes", and to delete the empty indexes whose status did answer while skipping the ones whose read failed.
- `config get` prints "not reported by the daemon" for a limit missing from the daemon's reply, instead of "unlimited" (#9214).
