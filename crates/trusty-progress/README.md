# trusty-progress

Shared rustup/cargo-style progress and status rendering for Trusty command-line
tools. It centralizes terminal detection, plain/quiet output, narrators,
component tables, spinners, progress bars, and human-readable byte formatting.

Add the published crate to a Rust project:

```toml
[dependencies]
trusty-progress = "0.3"
```

Workspace packages should use the workspace dependency instead:

```toml
[dependencies]
trusty-progress = { workspace = true }
```

## Quick start

```rust
use trusty_progress::{Component, ComponentTracker, Mode, Narrator, Output};

let (output, capture) = Output::for_capture(Mode::Plain);
Narrator::new(output.clone()).info("installing components")?;

let mut tracker = ComponentTracker::new(output);
tracker.add(Component::new("trusty-search", 8_870_953));
tracker.print()?;

assert!(capture.contents().contains("trusty-search installed"));
# Ok::<(), trusty_progress::ProgressError>(())
```

The public API and a longer example are documented in
[`src/lib.rs`](src/lib.rs). The library has no runtime configuration; callers
choose an [`Output`](src/output.rs) mode and destination.

## Development

```bash
cargo check -p trusty-progress
cargo test -p trusty-progress --no-fail-fast
cargo clippy -p trusty-progress --all-targets --all-features -- -D warnings
```

Licensed under the [MIT License](../../LICENSE).
