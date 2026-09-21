Documentation
- `rust-build-performance` skill states the crate-edge rule: never add a workspace crate as a `[dev-dependencies]`/`[build-dependencies]` entry absent from the consumer's normal dependency tree, with the `cargo tree -e dev` check.
