Documentation

- `BASE-AGENT.md` gains an "Effort Matches Blast Radius" section: verification
  effort is proportional to what the change can break, consolidation ships
  inside the next change that touches that code rather than as a standalone
  cleanup, and a defect in code you are already editing is fixed now.
- `rust-engineer.md` carries the two Rust traps already in `CLAUDE.md` —
  `--no-fail-fast` on every `cargo test` (cargo stops issuing targets on the
  first target failure, so the counts overstate coverage; #5324, PR #5904), and
  `trusty-common` needing `--features` on every test run because its default
  feature set is empty.
