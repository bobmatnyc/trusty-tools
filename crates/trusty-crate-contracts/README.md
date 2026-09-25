# trusty-crate-contracts

Test-only workspace member. It holds the tests that span two production crates,
so that neither production crate has to dev-depend on the other.

Never published (`publish = false`); it exposes no library surface.

## Why it exists

The point of separate crates is an efficient compilation process (owner ruling,
2026-09-21). A cross-crate test parked inside one of the two crates it spans is
paid for with a `[dev-dependencies]` edge on the other, and cargo charges that
edge to every `cargo test -p <crate>` and every
`cargo clippy -p <crate> --all-targets` in every worktree — not only to the
runs that care about the cross-crate claim.

Measured on #8341, before the move:

| Edge | Workspace crates the dev edge added |
|---|---|
| trusty-mpm → trusty-review | 8 |
| trusty-analyze → tga, trusty-audit, trusty-installer, trusty-console | ~135 |
| trusty-memory → trusty-installer | 13 |

Here the same tests run over NORMAL dependencies on both sides, so the cost
lands on whoever runs this crate.

## What is in it

| Test | Contract |
|---|---|
| `tests/conformance_cross_gate.rs` | AC-18 (DOC-15 C5, #1362): trusty-mpm's FRONT gate and trusty-review's BACK gate never disagree about one shared `ResolvedIntent`. |
| `tests/analyze_uds_consumers.rs` | #6287: one live trusty-analyze socket, and all four of its consumers agree about what they see. |
| `tests/memory_uds_consumer.rs` | #6555: tctl's own probe reads a live trusty-memory daemon as `Serving`. |

## Adding a test here

A test belongs here when it needs the real entry points of two crates that do
not already depend on one another. A test that needs only its own crate stays
in that crate, where it is cheaper to run.

Declare what the test needs as a NORMAL dependency. A `[dev-dependencies]`
section in this crate would be the exact shape
`scripts/check_dev_dep_edges.sh` exists to refuse.

## Running it

```bash
SKIP_UI_BUILD=1 cargo test -p trusty-crate-contracts --no-fail-fast
```

`SKIP_UI_BUILD=1` because `trusty-console` has a `build.rs` that builds its
Svelte UI and would otherwise need pnpm on `PATH`. CI sets it globally.

This crate is deliberately absent from the root `default-members` list: a bare
`cargo build` at the workspace root should not compile the union of eight
daemons to satisfy a test suite nobody asked for.
