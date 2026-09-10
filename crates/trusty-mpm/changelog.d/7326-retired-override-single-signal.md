Fixed

- A launch with a leftover retired `.trusty-mpm/` override file logged the "RETIRED override file is NO LONGER READ" ERROR twice, because one launch resolves the PM prompt twice — once for the `prepare_session` stash and once for the launch itself. The emitter now reports each file at most once per process, keyed by path so a second project still reports its own leftover (#7326).
