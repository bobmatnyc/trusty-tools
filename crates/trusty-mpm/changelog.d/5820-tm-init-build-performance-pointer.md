Added

- `/tm-init` now adds a **Build Performance** pointer to a Rust project's
  scaffolded or refreshed `CLAUDE.md` (keyed off the same `Cargo.toml` marker
  `rust-engineer` deploys on), so build-performance discipline is part of
  standard project setup instead of something reached only after a build
  already feels slow. The pointer references the bundled
  `rust-build-performance` skill and directs a new contributor to
  `cargo build --timings` for a measured baseline. It deliberately does not
  assert that `sccache` (or any shared compilation cache) makes builds
  faster: for a workspace's own path/member crates — the common
  multi-worktree cold-build scenario — sccache under its default config gets
  zero benefit, since cargo's dev profile builds those crates incrementally
  and sccache cannot cache incremental output. `CARGO_INCREMENTAL=0` is
  named as the only lever for a cross-worktree hit on path crates, without
  recommending it — that tradeoff is unmeasured. Whether sccache pays off on
  a workspace's external, non-path dependencies stays a separate, unanswered
  question the pointer directs the operator to measure (`sccache
  --show-stats`) rather than assume.
