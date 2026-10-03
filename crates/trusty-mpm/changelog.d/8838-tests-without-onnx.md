Performance
- A default `cargo test -p trusty-mpm` no longer compiles ONNX (`ort`, `ort-sys`, `fastembed`) or trusty-common's `memory-core`. The test-only `[dev-dependencies]` entry stopped turning `memory-core` on; the memory-palace tests still run under `--features sm-memory` and `--features manager-memory`, which CI's affected-crates lane now runs for trusty-mpm.
