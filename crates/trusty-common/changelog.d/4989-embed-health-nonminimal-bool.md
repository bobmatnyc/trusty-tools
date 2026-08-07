Changed

- `PalaceHandle::embed_health` states its liveness filter as
  `!expired || tier_c` instead of `!(expired && !tier_c)`. Same drawers, same
  order — De Morgan on the guard clippy flagged as `nonminimal_bool`, which
  broke `cargo clippy -p trusty-common --features memory-core --all-targets
  -- -D warnings` on `main`. The new form also reads as the doc comment already
  described it: keep a drawer unless it is expired and not Tier C
