Fixed
- The auto-resume breaker tests no longer write `/tmp/.trusty-mpm/scrollback.txt`, and the ancestor `CLAUDE.md` tests no longer resolve their project root to a `/tmp` that holds a `.trusty-mpm/` directory, so `cargo test -p trusty-mpm` stays green when run twice on one Linux host (Refs [#8838](https://github.com/bobmatnyc/trusty-tools/pull/8838)).
