Fixed
- The stale-socket tests no longer flake under a full `cargo test -p trusty-mpm` run: they wait until a dial to the dropped socket is refused, so a listener fd inherited by a concurrently forked child cannot make the dial connect and reset (#10026).
