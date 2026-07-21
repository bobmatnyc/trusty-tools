# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Fixed

- `agents::builder::merge_frontmatter` now quotes an emitted scalar frontmatter value (`name`, `role`, `description`, `model`, `resource_tier`) whenever it needs quoting to stay valid YAML — e.g. a `description` containing a colon (closes #3556). Previously every scalar was emitted as a bare plain YAML scalar regardless of content, even though `split_frontmatter` had already stripped any quotes the source template used; a description like `Rust 2024 edition specialist: memory-safe systems` composed to an UNQUOTED line a strict YAML parser (`serde_yaml`, used by `trusty-agents`' `.md` agent loader) rejects with "mapping values are not allowed in this context", while trusty-mpm's own lenient reader tolerated it — so the bug was invisible to trusty-mpm's own tooling. Composition was invariant to source-side quoting, so re-provisioning alone could never have fixed an affected agent; this is a fix to the shared composer, not merely a re-deploy. `split_frontmatter` now symmetrically decodes the same escaping on parse so a compose → deploy → re-compose cycle round-trips verbatim.

### Added

- `agents::frontmatter::validate_frontmatter`: strict-parses a document's frontmatter block with `serde_yaml` — the same check a real consumer (e.g. `trusty-agents`' `.md` agent loader) applies, deliberately stricter than the crate's own lenient `parse_kv_line` grammar (#3556). `agents::deployer::deploy_agents_filtered` now calls it on every freshly composed agent before writing: a composition that fails strict validation is treated like a compose failure — logged loudly, recorded in `DeployResult::failed`, and skipped — so a malformed agent is caught at deploy time instead of silently landing in `.claude/agents/` and only failing at runtime.

## [0.2.3] — 2026-07-19

### Fixed

- `events::tests::publish_round_trips_through_subscribe` (and sibling tests `bus_is_singleton`, `seq_is_monotonic`) now carry `#[serial_test::serial]` — these three tests share the process-global `HarnessEvent` broadcast bus, so a concurrent workspace test run could deliver an unrelated event or interleave `publish()` sequence numbers between them, causing an intermittent assertion failure (closes #2961; same pattern as the #2271 `bm25_client` fix).

### Added

- new public `transport` module (`EventSource`, `MembershipProvider`, `SourceEvent`, `EventEnvelope`, `aggregate_live`): the harness-agnostic multi-client attach/fan-out transport extracted from `trusty-code::workstreams::sse` (issue #3299, DOC-48 §5.3.1/AC-7, epic #3292; enables trusty-agents epic #3052 adoption). Generic over an opaque group id and event payload — zero axum/tcode dependency, HTTP framing stays in each consumer. `trusty-code`'s `workstreams::sse` now implements `EventSource`/`MembershipProvider` over `crate::events`/`SharedWorkstreamStore` and delegates to `transport::aggregate_live`; behavior is unchanged (same test suite, ported unmodified). Additive only.
- new public `agents::builder_in_memory` module (`InMemorySources`, `build_in_memory_source_map`, `compose_agent_in_memory`): an in-memory counterpart to `agents::builder::compose_agent` that resolves `extends:` inheritance chains (including through BASE-* templates) against an embedded `name -> markdown` asset map instead of a filesystem `source_dir` (refs #2958, epic #2892 Slice E1 — the foundation for trusty-code embedding a curated tm agent roster as `include_str!` assets). Internally, `agents::builder::resolve` was generalised over a new `pub(crate)` `SourceLookup` trait so both the fs and in-memory paths share one chain-walk/cycle-detection/depth-limit implementation instead of forking it; the fs `compose_agent` API and its behavior are unchanged (verified by a byte-equivalence test comparing fs vs. in-memory composition of identical content). Additive only.
- `agents::builder::Frontmatter` (and the public `agents::metadata::AgentMetadata` it projects into) gains two fields — `max_tokens: Option<u32>` and `tools: Option<Vec<String>>` — making the shared frontmatter type a superset of tcode's TOML `AgentConfig` (refs #2897, epic #2892, Slice A). `max_tokens:` merges scalar child-wins across an `extends` chain, identically to `model:`. `tools:` merges by OVERRIDE (a child whose `tools:` key is present replaces the parent's list entirely) — deliberately distinct from `skills:`'s union/accumulate merge, so a restrictive leaf agent can narrow a permissive base's tool set. `tools:` is `Option`, not a bare `Vec`, so an omitted key (`None` → inherit the parent) stays distinguishable from an explicit `tools: []` (`Some(vec![])` → deny-all override) — mirrors tcode's `ToolsConfig.allowed: Option<Vec<String>>`. Purely additive and behavior-preserving for trusty-mpm: `tm`'s agents never set either key, so composed output for a tm-style agent stays byte-identical.

## [0.2.2] — 2026-07-17

### Added

- new public `agents` module (`agents::builder`, `agents::deployer`, `agents::manifest`, `agents::frontmatter`): the `extends:`-inheritance agent composer, the checksum + atomic-write ownership manifest, and the deploy pipeline extracted from `trusty-mpm`'s binary crate for cross-crate reuse (refs #2892) (closes part of the #2892 extraction). `agent_manifest`'s error type is now a self-contained `ManifestError` (thiserror) so the shared crate carries no host-crate dependency. Additive only — no breaking changes to existing exports ([#2909](https://github.com/bobmatnyc/trusty-tools/pull/2909)) ([`bb947ea`](https://github.com/bobmatnyc/trusty-tools/commit/bb947ead9e220a37b8902b1190d261295c23538b))
- new public `skills` module (`skills::deployer`, `skills::manifest`, `skills::tiers`): the skills deploy/manifest/tiers machinery extracted from `trusty-mpm`'s binary crate for cross-crate reuse (refs #2892, #2818). Additive only — no breaking changes to existing exports ([#2916](https://github.com/bobmatnyc/trusty-tools/pull/2916)) ([`488602d`](https://github.com/bobmatnyc/trusty-tools/commit/488602dfa5cc75916f33c66b555832ce310b0025))

## [0.2.1] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).

## [Unreleased]

### Changed

- hoist compress::tool_output from trusty-agents ([#1959](https://github.com/bobmatnyc/trusty-tools/pull/1959)) ([#1968](https://github.com/bobmatnyc/trusty-tools/pull/1968)) ([`7cf93b9`](https://github.com/bobmatnyc/trusty-tools/commit/7cf93b9ab3918aff316238bdfe540a4053aa971d))
- publish trusty-agents-common 0.1.3 + trusty-mpm 0.11.0 to crates.io ([#1750](https://github.com/bobmatnyc/trusty-tools/pull/1750)) ([`70194ec`](https://github.com/bobmatnyc/trusty-tools/commit/70194ec1788fed2e71016912dae4e062baade139))
