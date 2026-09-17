Fixed

- The two embedded-roster tests that compose `rust-engineer` no longer pin a
  skill name from the shared agent asset (#8215). Both asserted the composed
  body contained `toolchains-rust-core`, which #8192 replaced with
  `rust-delivery-workflow` in `rust-engineer.md`, so `cargo test -p trusty-code`
  was red on main. The leaf-tier marker is now rust-engineer's own
  `# Rust Engineer` heading, which tracks the agent's identity instead of its
  skill wiring.
