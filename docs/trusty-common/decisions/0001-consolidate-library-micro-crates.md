# 0001. Consolidate library micro-crates into one feature-gated crate

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Crate `trusty-common`
- **Supersedes / Superseded by:** —

## Context

The trusty-* ecosystem grew as a constellation of small published crates:
`trusty-mcp-core` (JSON-RPC/MCP envelopes), `trusty-rpc` (a JSON-RPC client),
`trusty-embedder` (the fastembed/ONNX abstraction), `trusty-symgraph` (the
tree-sitter symbol graph), `trusty-memory-core` (the Memory-Palace storage
engine), `trusty-tickets` (the GitHub/JIRA/Linear MCP server), and
`trusty-monitor-tui` (the ratatui dashboards) — on top of a base `trusty-common`
utilities crate.

This factoring caused three recurring problems:

1. **Drift.** The *same* `Request`/`Response` MCP envelope, the *same* `Embedder`
   trait, and the *same* launchd plist generator were copy-pasted across
   trusty-memory and trusty-search and diverged subtly — cache vs no-cache
   embedders, drifting JSON-RPC error-code types, three different ways to read the
   user's UID. A bug fixed in one copy stayed unfixed in the others.
2. **Release friction.** Each crate carried its own version, and cross-crate
   development required a `[patch.crates-io]` dance to test an unreleased change
   in a downstream crate.
3. **Coordination cost.** Seven version numbers to reason about, seven changelogs,
   seven `cargo publish` invocations for what was logically one shared library.

The naive fix — merge everything into one crate — has an obvious failure mode:
forcing every consumer to compile every dependency. A chat-only CLI must not pull
in ONNX Runtime; a `lexical_only` search index must not compile tree-sitter
grammars; a tool that never serves HTTP must not link axum + tower (the recurring
mistake later corrected in #226 and #249 for downstream crates).

This work spans the consolidation tracker
[#5](https://github.com/bobmatnyc/trusty-tools/issues/5) (phases 2c symgraph, 2d
memory-core), plus the absorption of ticketing
([#216](https://github.com/bobmatnyc/trusty-tools/issues/216) is the help-system
companion), the monitor TUIs
([#31](https://github.com/bobmatnyc/trusty-tools/issues/31),
[#34](https://github.com/bobmatnyc/trusty-tools/issues/34),
[#32](https://github.com/bobmatnyc/trusty-tools/issues/32)), the embedder-client
unification ([#110](https://github.com/bobmatnyc/trusty-tools/issues/110),
[#164](https://github.com/bobmatnyc/trusty-tools/issues/164)), and the migration
kernel ([#179](https://github.com/bobmatnyc/trusty-tools/issues/179)).

## Decision

We will consolidate the seven library micro-crates into **one crate,
`trusty-common`**, where **every absorbed subsystem sits behind an opt-in feature
flag** and **`default = []`**.

- The default build compiles only the always-on core (port-walk, data-dir,
  daemon-addr, tracing, chat, setup helpers) and its light dependencies
  (`tokio`, `serde`, `reqwest`, `tracing`, `sysinfo`, `dirs`, `colored`).
- Each heavy subsystem is reachable **only** through its feature: `mcp`, `rpc`,
  `embedder` (+ ORT/candle variants), `embedder-client`, `bm25`, `bm25-client`,
  `migrations`, `symgraph` / `symgraph-parser` / `symgraph-server`, `memory-core`
  (+ `-kuzu` / `usearch-migrate` / `sqlite-kg`), `tickets`, `cli-help`,
  `monitor-tui`, and `axum-server`.
- The symbol-graph engine is split so the **pure-data contracts** (`EntityType`,
  `RawEntity`, `EdgeKind`) ship under `symgraph` without tree-sitter, while the
  full parser lives behind `symgraph-parser` — which alone claims the
  `links = "tree-sitter"` native slot, enabled in at most one crate per build.
- Crates that previously published a separate name (e.g. `trusty-memory-core`) are
  reduced to thin re-export shims so existing import paths still resolve.
- The crate keeps a single `version` field (currently `0.8.0`) and remains
  publishable to crates.io under Elastic-2.0, so external consumers can adopt a
  single subsystem by enabling just its feature.

## Consequences

- **Positive:** one source of truth per shared primitive — a bug fixed in the
  `Embedder` trait or the MCP envelope is fixed everywhere at once; behavioural
  drift between the daemons is eliminated.
- **Positive:** cross-crate development no longer needs `[patch.crates-io]`; Cargo
  resolves the in-tree path automatically and workspace builds are atomic.
- **Positive:** the `default = []` gate means consolidation does **not** tax
  consumers — a chat-only consumer never compiles ONNX, tree-sitter, redb, or
  axum; the dependency surface each crate carries shrank rather than grew.
- **Positive:** one version to bump for shared code, one changelog, one publish.
- **Negative / trade-off:** `Cargo.toml` carries a large, intricate `[features]`
  table with non-obvious implications (ORT variants are mutually exclusive at the
  `ort-sys` level; `memory-core` auto-enables `embedder` + `embedder-bundled-ort`;
  `symgraph-parser` monopolises the tree-sitter `links` slot). The feature matrix
  is now itself a thing to maintain and document (see ARCHITECTURE §2).
- **Negative / trade-off:** a few transitional features (`sqlite-kg`,
  `usearch-migrate`) exist only to read legacy stores during one-shot upgrades and
  must be remembered for later removal (#47, #51).
- **Neutral:** documentation drift risk — the in-crate `README.md` lagged the
  `Cargo.toml` (still showing v0.3 without `memory-core`/`monitor-tui`/
  `embedder-client`/`tickets`/`cli-help`); this spec set reflects the audited tree
  and #430 tracks the broader inventory reconciliation.
