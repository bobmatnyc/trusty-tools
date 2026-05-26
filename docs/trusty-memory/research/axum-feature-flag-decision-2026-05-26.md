# Decision: axum-server Feature Flag for trusty-memory

**Date**: 2026-05-26
**PR**: #240 (`ab8ebbc`)
**Issue**: #226

## Why

The trusty-tools workspace rule (CLAUDE.md, "Feature flags" convention) requires
that `axum` and `tower-http` be gated behind an `axum-server` feature in any
crate that is also consumed as an rlib. The motivation: pulling in axum
unconditionally forces every downstream consumer to compile the full axum +
tower + hyper stack even when that consumer only needs the in-process logic.

The immediate trigger was `open-mpm`, which links `trusty-memory` as an rlib
to access the in-process MCP tool dispatch layer. Before this change, `open-mpm`
had to carry axum and tower-http in its dependency tree despite never binding
an HTTP socket — increasing compile times and binary size for no benefit.

The workspace already established this pattern in `trusty-common`, which has
gated its own axum helpers behind `axum-server` since the initial workspace
consolidation.

## What Was Changed

`axum`, `tower-http`, and the SSE/REST handler modules in `trusty-memory` are
now compiled only when the `axum-server` feature is enabled. The feature is
**default-enabled**, so existing binary builds (`cargo build -p trusty-memory`,
`cargo install trusty-memory`) require no changes.

The `[features]` table in `crates/trusty-memory/Cargo.toml` now reads:

```toml
[features]
default = ["axum-server"]
axum-server = ["dep:axum", "dep:tower-http"]
```

## How to Use

**Binary / full daemon** (default — no change needed):
```toml
trusty-memory = { workspace = true }
```

**rlib consumer — no HTTP stack** (e.g. `open-mpm`, test harnesses):
```toml
trusty-memory = { workspace = true, default-features = false }
```

With `default-features = false`, the crate compiles the storage engine, MCP
tool dispatch, and knowledge-graph layer but omits the axum router, SSE
endpoint, and REST API handlers.

## Trade-offs

The default-enabled arrangement means the common case (building or installing
the daemon binary) works without any `Cargo.toml` changes. The trade-off is
that a developer who forgets `default-features = false` when writing a
lightweight consumer will silently pull in axum — but this is the same
behaviour as before the change, so it is not a regression. The feature flag
makes the opt-out explicit and documented rather than impossible.

Any code gated on `#[cfg(feature = "axum-server")]` is not reachable from
consumers that disable the feature; the compiler will reject attempts to call
into axum-gated functions from `default-features = false` dependents, providing
a clear build-time error rather than a silent footgun.
