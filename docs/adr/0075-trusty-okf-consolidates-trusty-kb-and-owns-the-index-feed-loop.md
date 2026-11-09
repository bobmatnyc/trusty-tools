# 0075. Generic `trusty-kb` code consolidates under `trusty-okf`, which replaces the planned `trusty-okg` name and owns the index-feed loop — amends ADR-0041

- **Status:** Accepted
- **Date:** 2026-11-03
- **Accepted:** 2026-11-03 (owner, Bob). Naming: ruling 03:06Z on [SPEC-OKF-01](../specs/SPEC-OKF-01.md) §3 (a), "consolidate generic trusty-kb functionality under trusty-okf, superseding planned trusty-okg naming; preserve agent glue boundary". Feed loop: ruling 06:58Z, "all recommended", which decides §3 (e) option A. SPEC-OKF-01 r7.1 accepted as written 09:16Z.
- **Scope:** crate `trusty-kb` (retires); new crate `trusty-okf` (`crates/trusty-okf`, SPEC-OKF-01 §7); `crates/trusty-agents` (call sites, retained agent glue, and `src/stores/index_feed.rs` / `index_feed_rpc.rs`, which move out); `trusty-search` (keeps chunking, embedding and ranking; no OKF indexing loop); ADR-0041 (amended)
- **Reversibility Cost:** Low — `trusty-kb` was never released, so no published name changes. The cost is the call-site move in trusty-agents and the feed-loop move.
- **Decision Drivers:** owner rulings 2026-11-03 03:06Z and 06:58Z; SPEC-OKF-01 §1 requirement that trusty-okf owns building and maintaining OKF repositories; ADR-0040's consumer criterion; the agent-glue boundary; trusty-search has no OKF indexing loop today (SPEC-OKF-01 §5.5)
- **Supersedes / Superseded by:** — (amends [ADR-0041](0041-trusty-okg-native-crate-search-absorbs-okf-indexing.md): Decision 1's `trusty-okg` name and rename path, and Decision 2's feed-loop ownership)
- **Spec:** [SPEC-OKF-01](../specs/SPEC-OKF-01.md) §2.6, §2.7, §2.12, §3 (a), §3 (e), §5, §6.1 item 9
- **Issue:** [#10743](https://github.com/bobmatnyc/trusty-tools/issues/10743) (tracker [#10737](https://github.com/bobmatnyc/trusty-tools/issues/10737))

**Provenance.** This ADR files the trusty-okf project's proposal ADR-OKF-0001 and adds the §3 (e) feed-loop decision that SPEC-OKF-01 §6.1 item 9 names as a further amendment. The proposal, the spec reviews, the acceptance reconciliation, the owner-choices list and the readback stay in the trusty-okf project's `docs/specs/` and `docs/adr/`. Citations of the form `path:line@c1eed98441` refer to the trusty-tools checkout at that commit.

## Context

### What ADR-0041 decided

ADR-0041 (Accepted 2026-08-10) made five decisions (`docs/adr/0041-…md:80-86@c1eed98441`):

1. `trusty-kb` is renamed `trusty-okg` and stays a native crate.
2. Indexing responsibility moves to trusty-search, which gains OKF awareness and takes over the reconcile-and-push loop of `index_feed.rs`.
3. Each OKG store is a distinctly named trusty-search index.
4. A new service in trusty-mcp-services fronts agent-consumed reads, and depends on the native crate as a library.
5. The rationale is ADR-0040's consumer criterion, applied twice.

The rename in Decision 1 has not happened. The crate is still `crates/trusty-kb`, at v0.1.0 with `publish = false`. Decision 2 has not happened either: trusty-search has no OKF awareness and no feed loop (SPEC-OKF-01 §5.5).

### Why the naming is amended

On 2026-11-03 Bob set up trusty-okf as a separate Rust crate that owns building and maintaining OKF repositories (SPEC-OKF-01 §1). That role overlaps Decision 1 directly. Two crates, `trusty-okf` and `trusty-okg`, would split one capability.

SPEC-OKF-01 §2.7 surveyed the code. Almost all of `trusty-kb` is generic OKF bundle and build code: format, container rules, store operations, ingest, ledger, trust labels and the index journal. The agent-specific parts are small: the agent-slug root mapping in `roots.rs`, and the `kb_*` MCP server. With the generic code in trusty-okf, a separate `trusty-okg` crate would hold only agent glue, which already has a home in trusty-agents.

### Why the feed-loop ownership is amended

SPEC-OKF-01 §3 (e) asked who owns the loop that pushes concept files to trusty-search and withdraws them. Option C was ADR-0041 Decision 2: trusty-search absorbs OKF indexing. That depends on trusty-search work that is not built. Option A puts the loop in trusty-okf, in a feature-gated module moved from `crates/trusty-agents/src/stores/index_feed.rs`. The loop is generic, and ADR-0041 already wants it out of trusty-agents. Option A contradicts Decision 2 for the feed loop, so SPEC-OKF-01 §6.1 item 9 requires this amendment before implementation.

### The owner rulings

- Bob, 2026-11-03 03:06Z, after SPEC-OKF-01 §3 (a) recommended option A: "Let's consolidate and then give me the review." The Architect's brief records the scope: "consolidate generic trusty-kb functionality under trusty-okf, superseding planned trusty-okg naming; preserve agent glue boundary."
- Bob, 2026-11-03 06:58Z: "all recommended" on the 26 owner choices. Item 14 is §3 (e); the recommended option is A.
- Bob, 2026-11-03 09:16Z: SPEC-OKF-01 r7.1 accepted as written. Design only; implementation starts when Bob schedules it.

## Decision

1. **We consolidate generic `trusty-kb` code under `trusty-okf`.** Every generic module moves to trusty-okf per SPEC-OKF-01 §2.7.2. `trusty-kb` retires after the cut-over.
2. **`trusty-okf` supersedes the planned `trusty-okg` name.** No crate named `trusty-okg` is created. "OKG" stays the name of the built, per-agent bundle (DOC-63), not of a crate.
3. **Agent glue stays in trusty-agents.** Agent glue is code that knows the trusty-agents home layout, agent names or slugs, agent configuration, credentials, connectors or fence rendering.
4. **The boundary is a checkable rule.** trusty-okf must meet rules B1–B10 of SPEC-OKF-01 §2.7.1. In short:
   - trusty-okf has no dependency on trusty-agents, trusty-mcp, trusty-mcp-services, trusty-gworkspace or any connector crate.
   - trusty-agents depends on trusty-okf, never the reverse.
   - Every public entry point takes a bundle root path. No function derives a path from an agent name, slug, home directory or environment variable.
   - trusty-okf knows nothing about the agent home layout, and never creates, deletes or names a search index.
   - trusty-okf holds no credentials and defines no connector-specific `Locator` variant.
   - trusty-okf has no MCP surface of its own.
   - Network code sits behind the cargo feature `search-feed`, so the core stays network-free.
   - trusty-okf never links xberg (B9), and adds no credential to a child process it starts (B10; scope in SPEC-OKF-01 §2.7.1).
5. **trusty-okf owns the index-feed loop, behind a cargo feature (SPEC-OKF-01 §3 (e) A).** The reconcile-and-push loop of `crates/trusty-agents/src/stores/index_feed.rs` and `index_feed_rpc.rs` moves to the module `trusty_okf::feed`, compiled only with the cargo feature `search-feed` (rule B8). The feed pushes concept files to trusty-search, withdraws them, and reads results back through trusty-search's query method. SPEC-OKF-01 §5 is its contract.
6. **trusty-search keeps chunking, embedding and ranking.** trusty-okf never chunks, embeds or ranks (SPEC-OKF-01 §2.6). trusty-search does not take over the OKF feed loop. Gaps that need trusty-search changes are filed as external dependencies D1 and D2 (SPEC-OKF-01 §5.6), not built by trusty-okf.

This ADR decides nothing else. SPEC-OKF-01 §3 and §7 record the other owner decisions: OKF version policy, correction model, verification fields, public surface, publishing, deprecated-concept indexing, crate location and import scope.

## What this ADR replaces in ADR-0041, and what it keeps

| ADR-0041 text (`@c1eed98441`) | Disposition |
|---|---|
| Title (`:1`): "`trusty-kb` renamed `trusty-okg`, stays a native crate" | **Replaced.** Read: "`trusty-kb`'s generic code consolidates under `trusty-okf`, a native crate." |
| Decision 1 (`:82`): "`trusty-kb` is renamed `trusty-okg` and remains a native crate" | **Replaced in part.** The rename to `trusty-okg` is replaced by Decisions 1–3 of this ADR. "Remains a native crate — not an MCP service" is **kept** and applies to trusty-okf. |
| Decision 2 (`:83`): indexing moves to trusty-search, which takes over the reconcile-and-push loop | **Replaced in part.** The feed loop moves to trusty-okf's feature-gated `feed` module (Decision 5), not to trusty-search. trusty-search keeps chunking, embedding and ranking (Decision 6). The OKF-awareness work Decision 2 assigned to trusty-search is not adopted; SPEC-OKF-01 §5.5 and §5.6 list what trusty-search must still provide. |
| Decision 3 (`:84`): each OKG store is a distinct named index | **Kept.** trusty-agents still owns the binding and index creation. |
| Decision 4 (`:85`): a trusty-mcp-services service fronts agent reads and depends on the native crate | **Kept.** The service's dependency becomes `trusty-okf` instead of `trusty-okg`. `kb_put_entity` stays unresolved. |
| Decision 5 (`:86`): consumer-criterion rationale | **Kept.** |
| Context, two-layer split (`:38-41`): "`trusty-okg` (the renamed `trusty-kb`) — native crate" and the `trusty-okg = { path = "../trusty-okg" }` dependency | **Replaced** by `trusty-okf` and `trusty-okf = { path = "../trusty-okf" }` (SPEC-OKF-01 §7, decided A). |
| Consequences, Neutral (`:106`): `trusty-okg` names the tree while the crate implements the container | **Resolved.** The crate is now named for the format it implements. |
| Consequences, rename surface (`:100`): 51 call sites in 18 files, `Cargo.toml:84` | **Updated** below. |
| HTTP route text (`:62`, `:66`, `:93`) | **Not amended here.** It is stale since [#9214](https://github.com/bobmatnyc/trusty-tools/issues/9214) retired HTTP. The feed uses JSON-RPC over a Unix-domain socket (SPEC-OKF-01 §5.1). Flagged as a documentation follow-up. |

## Consequences

### Crate rename path from `trusty-kb`

The path below is a design sequence, not a scheduled implementation plan.

1. Create `crates/trusty-okf` with `publish = false` (SPEC-OKF-01 §3 (h) A).
2. Move each generic module with `git mv`, so file history survives (SPEC-OKF-01 §2.7.2).
3. Move the agent-slug root mapping (`crates/trusty-kb/src/roots.rs:25-65@c1eed98441`) into trusty-agents.
4. Leave the `kb_*` MCP server (`mcp.rs`, `tooldefs.rs`, `main.rs`) out of trusty-okf. Its binary is registered nowhere (ADR-0041 `:30`), so retiring it breaks no registered caller. The agent-consumed tools wait for the trusty-mcp-services service of ADR-0041 Decision 4.
5. Move `stores/index_feed.rs` and `index_feed_rpc.rs` into `trusty_okf::feed`, behind its cargo feature.
6. Switch trusty-agents from `trusty-kb` to `trusty-okf`.
7. Delete `crates/trusty-kb` and its workspace member entry (`Cargo.toml:75@c1eed98441`) once no crate depends on it.

`trusty-kb` never cut a release (ADR-0041 `:95`), so no crates.io yank or deprecation is needed. On-disk formats under `_sources/` stay readable, so existing OKG trees need no conversion for the rename itself. The OKF v0.2 migration is a separate step (SPEC-OKF-01 §2.8, §2.9).

### Deprecation of the `trusty-okg` name

- No crate, binary or package is ever published as `trusty-okg`.
- "OKG" stays the name of each agent's built bundle and of existing identifiers such as `okg_*` tool names, `okg/` directories and `assistant-okg-*` index IDs. This ADR renames none of them.
- Prose that says "`trusty-okg`" as a crate name becomes stale. Known sites: ADR-0041 throughout, and ADR-0022's `trusty-kb-<tree>` sync-repo naming, which ADR-0041 `:116` already flags. Updating them is documentation follow-up.

### Call-site impact in trusty-agents

At `c1eed98441`, outside `crates/trusty-kb`:

- 57 `trusty_kb::` lines in 20 `.rs` files, all in trusty-agents, plus one changelog fragment (`crates/trusty-agents/changelog.d/4532-4533-okg-trust-labels-and-fencing.md`).
- The heaviest files are `src/tools/okg/tests.rs` (6), `src/api/server/project_tools.rs` (5), `src/stores/status_tests.rs` (5), `src/stores/index_feed.rs` (4) and `src/tools/okg/mod.rs` (4).
- One Cargo edge: `crates/trusty-agents/Cargo.toml:111`. ADR-0041 cites `:84`; the line has moved.
- Type renames follow SPEC-OKF-01 §2.7.2, for example `KbStore` to `Bundle` and `Entity` to `Concept`. A re-export shim is not needed, because trusty-agents is the only consumer (inference from the grep).
- `Locator::Gmail` and `Locator::Drive` leave trusty-okf through DOC-55 §5.3's open variant. trusty-agents takes over those two kinds. Existing `registry.toml` rows must still load (SPEC-OKF-01 §2.9).
- `stores/index_feed.rs` and `index_feed_rpc.rs` move to `trusty_okf::feed` (Decision 5). Their callers in trusty-agents enable the feature.

### Positive

- One crate owns the OKF bundle, its build lifecycle and its index feed. This matches the owner's assignment in SPEC-OKF-01 §1.
- The crate name matches what it implements: the OKF format and its bundles.
- The feed-loop move needs no unbuilt trusty-search work. trusty-search stays a general search engine.
- The boundary is testable in review with `cargo tree` and grep, not by judgment.

### Negative and trade-offs

- trusty-okf carries build machinery (stores, ledger, ingest), not only the format. A future non-agent consumer gets that machinery too. Alternative B below would have avoided it.
- The `Locator` change touches the registry format that existing trees use. It needs a read-compatibility test.
- trusty-okf depends on trusty-search interfaces that do not exist yet: index identity (D1, [#10738](https://github.com/bobmatnyc/trusty-tools/issues/10738)) and path admission (D2, [#10739](https://github.com/bobmatnyc/trusty-tools/issues/10739)). Until D1 lands, the feed reports index state as "unconfirmed", never "done" (SPEC-OKF-01 §4.9).
- trusty-search's own OKF awareness (frontmatter as metadata, trust labels on chunks, DOC-63 S-4.4) stays a gap (SPEC-OKF-01 §5.5).

### Neutral

- DOC-63's owner decisions are untouched: one canonical OKG per agent at `<home>/<agent>/okg`, trust labels before connector grants, and a separate refresh runner.
- ADR-0040 is untouched. Its consumer criterion still makes the crate native.
- ADR-0022 is untouched by this ADR. The sync rule for `_sources/ops/` is a separate filing ([#10744](https://github.com/bobmatnyc/trusty-tools/issues/10744)).

## Alternatives considered

- **Naming B. trusty-okf is the format crate; trusty-okg is the agent build crate on top.** trusty-okg would hold stores, ledger, ingest, trust labels and the feed. Not chosen by the 03:06Z ruling. Bob assigned building and maintaining, not only the format, to trusty-okf. Once build code is generic, trusty-okg would hold only agent glue, which belongs in trusty-agents.
- **Naming C. Keep ADR-0041 as written and drop trusty-okf.** Not chosen. It contradicts the 2026-11-03 requirement for a separate crate named `trusty-okf`.
- **Feed loop B. trusty-agents keeps the loop.** Not chosen. The loop is generic, and ADR-0041 already wants it out of trusty-agents.
- **Feed loop C. trusty-search absorbs OKF indexing, per ADR-0041 Decision 2.** Not chosen. It depends on trusty-search work that is not built.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-11-03:

- **ADR-0041 (Native trusty-okg with search indexing): Amends.** Decision 1's name and rename path, and Decision 2's feed-loop ownership. Decisions 3–5 stand. ADR-0041's status becomes `Amended by 0075`.
- **ADR-0040 (Split MCP framework and services): Consistent.** The consumer criterion keeps trusty-okf native. Boundary rule B7 restates it.
- **ADR-0022 (Knowledge-tree synchronization): Consistent.** This ADR changes no sync rule; the sync rule is amended separately by [#10744](https://github.com/bobmatnyc/trusty-tools/issues/10744) (#10743). The `trusty-kb-<tree>` repo naming is stale prose.
- **ADR-0024 (Sub-agents are in-process leaves): Consistent.** A sub-agent gets the in-process import tier only, so ADR-0024 is not amended (SPEC-OKF-01 §2.12, owner-choices item 3 A).
- **ADR-0026 (A credential grant does not survive delegation): Consistent.** trusty-okf holds no grant (B6, B10). An authenticated source adapter's caller holds its own.
- **ADR-0029 (MSRV 1.94): Consistent.** The extraction worker builds outside the workspace (SPEC-OKF-01 §3 (n) B); recording that is a separate filing ([#10745](https://github.com/bobmatnyc/trusty-tools/issues/10745)).
- **ADR-0033 (Consolidate MCP services; Superseded by 0040): No interaction.**
- **DOC-55 (Universal OKG Importer, Draft): Consistent.** The `Locator` move uses its §5.3 open variant. The extraction worker needs a DOC-55 §8 amendment, filed separately ([#10741](https://github.com/bobmatnyc/trusty-tools/issues/10741)).
- **DOC-63 (OKG Sources, Accepted): Consistent.** The layout, binding and index creation stay in trusty-agents. The push feed stays the correctness path (S-8.1); only its owner changes. The S-3.3 SENT-only amendment is filed separately ([#10742](https://github.com/bobmatnyc/trusty-tools/issues/10742)).

No conflict with any other Accepted or Proposed ADR.

## References

- [SPEC-OKF-01](../specs/SPEC-OKF-01.md) r7.1 — §2.6, §2.7, §2.12, §3 (a), §3 (e), §5, §6.1 item 9
- trusty-okf project (outside this repository): ADR-OKF-0001 (the proposal this ADR files), and beside the spec the review, the acceptance reconciliation, the owner-choices list and the readback for Bob
- [`0041-trusty-okg-native-crate-search-absorbs-okf-indexing.md`](0041-trusty-okg-native-crate-search-absorbs-okf-indexing.md) — amended by this ADR
- [`0040-trusty-mcp-services-absorbs-trusty-gworkspace.md`](0040-trusty-mcp-services-absorbs-trusty-gworkspace.md)
- `crates/trusty-agents/src/stores/index_feed.rs`, `index_feed_rpc.rs` — the loop that moves to `trusty_okf::feed`
- Issues [#10737](https://github.com/bobmatnyc/trusty-tools/issues/10737) (tracker), [#10743](https://github.com/bobmatnyc/trusty-tools/issues/10743) (this amendment)
