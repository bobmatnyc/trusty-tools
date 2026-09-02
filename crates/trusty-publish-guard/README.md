# trusty-publish-guard

Internal release-safety check that detects local source drift under a crate
version already published to crates.io. It walks publishable workspace crates,
downloads the matching crates.io source archive when one exists, compares the
packaged source tree, and fails closed when parity cannot be verified.

Run it from the workspace root:

```bash
cargo run -p trusty-publish-guard --bin publish-guard -- --root .
```

The check skips packages marked `publish = false`. It exits nonzero when a
published version differs from the local source, crates.io cannot be checked,
or discovery falls below its safety floor. A mismatch normally means the
package version must be bumped before publication.

This package is an internal workspace tool and is not itself published. The
comparison engine is in [`src/lib.rs`](src/lib.rs); the crates.io fetcher seam
is in [`src/fetch.rs`](src/fetch.rs).

## Development

```bash
cargo check -p trusty-publish-guard
cargo test -p trusty-publish-guard --no-fail-fast
cargo clippy -p trusty-publish-guard --all-targets --all-features -- -D warnings
```

Network access is required only for a live parity run; tests use the fetcher
abstraction and do not require crates.io. The workspace is licensed under the
[MIT License](../../LICENSE).
