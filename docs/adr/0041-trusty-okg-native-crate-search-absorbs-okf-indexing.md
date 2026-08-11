# 0041. `trusty-kb` renamed `trusty-okg`, stays a native crate; `trusty-search` absorbs OKF indexing; agent-facing reads front it as a service in `trusty-mcp-services` — amends ADR-0040

- **Status:** Accepted
- **Date:** 2026-08-10
- **Scope:** crate `trusty-kb` (renamed `trusty-okg`); `crates/trusty-agents/src/stores/index_feed.rs` (indexing logic this decision moves out of `trusty-agents`); `trusty-search`'s indexer (destination for OKF awareness); `trusty-mcp-services` (new dependent — hosts the agent-facing OKG service); `crates/trusty-agents/src/tools/okg/*.rs` (unaffected — the native ingest `ToolExecutor`s stay); `.mcp.json`/`stable_set()`/`picker.rs` (affected only once the new service ships — not designed here)
- **Reversibility Cost:** Low-Medium — never published (free rename, see Consequences); the indexing-ownership move and the new service are more work than the rename (OKF-awareness code in `trusty-search`, retirement of `index_feed.rs`'s push logic, a new MCP service crate-internal to `trusty-mcp-services`) but touch no external contract, since nothing outside this workspace consumes either crate today
- **Decision Drivers:** owner ruling 2026-08-10 (verbatim below), reversing his own earlier MCP-service inclination on this crate, then resolving the read-path question the same day; the consumer criterion ADR-0040 already established (agent-consumed -> MCP, framework-consumed -> native), here applied twice over — once to the crate, once to the seam between capability and its agent-facing view; `trusty-search`'s existing named-index model already exposes the exact HTTP surface `index_feed.rs` calls
- **Supersedes / Superseded by:** **Amends ADR-0040** (`trusty-mcp` extracted from `trusty-common`; `trusty-mcp-services` absorbs `trusty-gworkspace`) — ADR-0040 explicitly reserved `trusty-kb`'s native-vs-MCP disposition as "the owner's unmade call." This ADR is that call, made in two parts across one day. Nothing else in ADR-0040 is touched: its `trusty-mcp`/`trusty-mcp-services` two-crate extraction, its `trusty-channels` correction, its consumer-criterion rule, its `inventory`-based service registry, and its WASM deferral all stand unmodified. **Note on numbering:** an earlier draft of this ADR was written against a same-day version of the `trusty-mcp-services` decision then numbered 0038; that number collided with an unrelated, already-`origin/main` ADR (`0038-kg-stays-additive-recall-gated-on-extraction-quality.md`) and the `trusty-mcp-services` ADR was renumbered to **0040** on branch `docs/adr-0040-trusty-mcp-services`. Every reference to "the trusty-mcp-services ADR" in this document means **0040**, not the unrelated KG-recall ADR that now holds 0038.

## Context

### ADR-0040 left this open, on purpose

ADR-0040 (Accepted, 2026-08-10, earlier the same day, at the time of its own writing numbered 0038) investigated whether `trusty-kb` was obsolete — it is not, its library is essential to `trusty-agents`' OKG feature — and separately investigated whether it belonged in the `trusty-mcp-services` consolidation ADR-0033 had proposed. It concluded:

> "**This ADR does not decide `trusty-kb`'s native-vs-MCP disposition** — the owner reserved that explicitly. But the decision rule above converts this from an open-ended question into an answerable one... The one fact for that future decision: its standalone MCP binary is unregistered anywhere today."

ADR-0040's own Open Questions list repeats it: "`trusty-kb`'s native-vs-MCP disposition — owner's unmade call. Verified input: library essential, standalone MCP binary unregistered." This ADR makes that call, reversing an earlier inclination toward keeping it (or folding it into) an MCP service, and then resolves the read-path question that call immediately raised.

### The reversal, stated so a reader sees it was reconsidered

The owner's earlier framing — recorded in ADR-0033 (now superseded) and echoed in ADR-0040's Context — treated `trusty-kb` as a candidate MCP-service consolidation target, grouped with `trusty-gworkspace` and `trusty-channels`. That framing is not what this ADR adopts. The owner's ruling today reverses it explicitly:

> **Owner, verbatim (2026-08-10):** *"let's keep it a native crate. There are many tools that won't be directly used by mcp, such as building a store, indexing a store etc. Let's make sure the indexing is done by search, and search should know how to index OKF. Keep the tool trusty-okg, and note that it's a native tool to build and index open knowledge stores. Like search, each store should be named and indexed distinctly."*

This is not a passive overtaking-by-circumstances (the ADR-0040 vocabulary for a decision later facts made moot) — it is the owner reconsidering the crate's shape and landing somewhere different from where ADR-0033 (and, implicitly, his own earlier "keep it MCP" instinct) had put it. Recorded here in the open, the same way ADR-0040 named ADR-0033's `trusty-channels` error in prose rather than only in the status field.

### The read-path question, and the owner's same-day resolution

Investigating this ruling surfaced a gap: splitting `trusty-kb`'s eight `kb_*` tools by ADR-0040's own consumer criterion put five of them on the agent-consumed side — `kb_get_entity`, `kb_list`, `kb_list_trees`, `kb_status`, `kb_convert_tree` (a model deciding to look something up, browse, or drive an import) — two cleanly on the framework side — `kb_ensure_structure`, `kb_validate` (deterministic maintenance, no judgment call) — and one straddling both — `kb_put_entity` (a model recording a fact is agent-consumed in principle, but as a bare tool it bypasses the `SourceSpec`/ledger/`TrustLabel` guardrails the `okg_ingest_*` path enforces, which is a materially less-guarded write than the one already built). A native-only crate with an unregistered binary would leave that agent-consumed majority exactly as unreachable as it is today — no CLI verb, no HTTP route, no GUI trigger reaches them, and `trusty-agents` today natively exposes only the write/ingest half (`okg_ingest_docstore`/`gmail`/`drive`/`sources`).

The owner resolved this the same day:

> **Owner, verbatim (2026-08-10):** *"trusty-okg will need an mcp service in trusty-mcp-services that talks to trusty-okg."*

This is a **two-layer split**, and it is the consumer criterion applied at the seam between a capability and its agent-facing view, not a second, unrelated judgment call:

- **`trusty-okg`** (the renamed `trusty-kb`) — native crate. Builds and indexes open knowledge stores. No MCP surface of its own, no daemon, no binary registration question to answer, because it never has agent-facing tools of its own.
- **A new service inside `trusty-mcp-services`** (the crate ADR-0040 already creates for `trusty-gworkspace`) — MCP-shaped, depending on `trusty-okg` as a library and fronting it. This is where the agent-consumed half lives: `kb_get_entity`, `kb_list`, `kb_list_trees`, `kb_status`, `kb_convert_tree`.

**The dependency this creates:** `trusty-mcp-services` gains a plain in-workspace path dependency on `trusty-okg` (`trusty-okg = { path = "../trusty-okg", version = "..." }`), the identical shape ADR-0040 already establishes for `trusty-gworkspace` inside the same crate — no new dependency mechanism, no dynamic loading, consistent with ADR-0040's outright rejection of `libloading`/`abi_stable` for exactly this kind of in-workspace service composition.

**Which of the eight tools cross, and which stay native-only:**

| Tool | Destination |
|---|---|
| `kb_get_entity`, `kb_list`, `kb_list_trees`, `kb_status`, `kb_convert_tree` | **Cross into the new `trusty-mcp-services` OKG service** — agent-consumed, per the split above |
| `kb_ensure_structure`, `kb_validate` | **Stay native-only inside `trusty-okg`**, called by the framework (ingest pipeline, future scheduled runner), never exposed as an MCP tool |
| `kb_put_entity` | **Flagged, not placed here.** Its bare form bypasses the ledger/`TrustLabel` guardrails `okg_ingest_*` already enforces (see above) — whether it belongs in the service (a model recording an ad hoc fact outside any registered source) or should be redesigned to route through the same guarded path before being exposed at all is unscoped follow-up, not decided by this ADR |

This is a general pattern, not a one-off for OKG: a native capability (deterministic, framework-consumed at its core) can still have an agent-consumed slice, and that slice's home is a thin MCP-shaped service in the services crate that depends on the native crate as a library — never the native crate growing its own MCP surface back. Future native capabilities needing agent access should follow this same seam rather than re-litigating MCP-vs-native for the whole capability.

### Applying the consumer criterion — twice, and each time it resolves

ADR-0040 established the rule: **consumer is the agent -> MCP; consumer is the framework -> native.** This ADR applies it twice. First, to the crate as a whole: building a store (walking a doc-store directory, writing entity files, maintaining the ledger) and indexing a store (pushing entity content into a search index) are deterministic, scheduled-or-triggered-by-ingest, no-model-judgment operations — framework-consumed, hence `trusty-okg` stays native. Second, to the seam within that capability: the read/browse/status/convert operations ARE agent-judgment-driven even though the capability underneath is native — hence a thin MCP service, not a wholesale reversal of the native call. The two applications do not conflict; they operate at different granularities of the same rule.

### What `index_feed.rs` does today, and what moves

`crates/trusty-agents/src/stores/index_feed.rs` (232 SLOC) is the piece this decision's "indexing is done by search" statement targets. Concretely, today it:

1. **Preflights index coverage** (`covers()`, lines 111-114) — refuses to push into an index whose registered root does not contain the OKG tree, because `trusty-search` post-filters any result outside the index root (issues #64/#541) and a blind push would manufacture a silent "stored but unfindable" failure.
2. **Calls the exact HTTP surface `trusty-search` already exposes for any index** — `POST /indexes/{id}/index-file` and `POST /indexes/{id}/remove-file` (`HttpIndexFeed`, lines 124-189), the same routes `trusty-search add`/`remove` and the `index_file`/`remove_file` MCP tools use for ordinary code repos (`crates/trusty-search/src/commands/{add,remove}.rs`, `crates/trusty-search/src/mcp/tools/index.rs:68,86`). Nothing about the wire contract is OKG-specific.
3. **Reconciles a per-source backlog** (`feed_source`, lines 243-300+) against `trusty-kb`'s ledger/journal (`KbStore::okg_index_backlog`, `IndexJournal`) to decide *what* needs pushing, withdrawing, or is already synced — this reconcile logic is pure and lives in `trusty-kb` already (`crates/trusty-kb/src/okg/index_journal.rs`); only the network call lives in `trusty-agents`, "for the same reason the Gmail/Drive fetchers do... `trusty-kb` stays deterministic and network-free" (module doc, `index_feed.rs:13-19`).
4. **Orders push-then-record and withdraw-before-replace** (module doc, points 3 and 5) so a crash mid-push costs at most one redundant re-push, never a permanently-unindexed entity.

**What trusty-search's index model already expresses:** `trusty-search index <path> --name <name>` (`crates/trusty-search/src/commands/index.rs`, `main.rs:269`) already registers a directory as a distinctly-named index with its own root, and the per-file incremental routes (`index-file`/`remove-file`) already accept arbitrary content by path, independent of the index's own root — which is exactly the shape `HttpIndexFeed` already calls. **The wire-level named-index model does not need new capability to serve an OKG store** — an OKG tree can be registered today with `trusty-search index <okg-tree-path> --name <agent>-okg` exactly as any code repo is, and per-entity content can already be pushed by path.

**What is genuinely new — the OKF-awareness the owner is asking for:** `trusty-search`'s crawler treats a directory's files as generic text/code content by extension and mode (`code`/`text`/`data`); it has no concept of OKF's container rules. Concretely, indexing OKF natively would require `trusty-search` to:
- Recognize and skip OKF's reserved filenames (`index.md`, `log.md` — `crates/trusty-kb/src/schema.rs:52-56`) as generated/non-entity, the way `KbStore`'s own listing already does (`store.rs:157`, `is_reserved_file`) — a generic crawl does not know this convention and would index them as ordinary prose.
- Parse and expose OKF frontmatter (`type`, `tags`, `same_as`, `sources`) as structured, filterable metadata rather than folding it into opaque chunk text — `trusty-search` has no frontmatter-aware ingestion path today.
- Read and respect `TrustLabel` (`crates/trusty-kb/src/okg/trust.rs`) at retrieval time — `trusty-search` has no per-chunk trust/fencing concept today; the closest analog, `trusty_agents::untrusted`, fences memory drawers, not search chunks, and is in a different crate entirely.
- Take over the reconcile-and-push loop `index_feed.rs` currently drives — either by crawling/watching a registered OKF tree directly (the way it already crawls a registered code repo), consuming `trusty-okg`'s ledger/journal as the "what changed" signal instead of `trusty-agents` calling per-file push/remove after each ingest.

None of this exists in `trusty-search` today. This ADR authorizes the direction — indexing ownership moves to `trusty-search`, and `trusty-search` gains OKF awareness — without designing the mechanism; that is real, unscoped follow-up work, consistent with how ADR-0040 left its own `trusty-mcp-services` packaging particulars for later implementation.

### Each store named and indexed distinctly — the OKG equivalent of `trusty-search`'s model

The owner's instruction — "each store should be named and indexed distinctly" — mirrors `trusty-search`'s existing convention: every registered index has a distinct `id`/`name` and its own root (`trusty-search index <path> --name <name>`, `crates/trusty-search/src/commands/index.rs`). The direct OKG equivalent, given what already exists: each per-agent OKG tree (`<knowledge_dir>/<slug(agent)>`, `stores/binding.rs:52-62`) registers as its own named `trusty-search` index — precisely the binding shape `AgentStoreBinding`'s `tree`/`index` pair already assumes (`crates/trusty-agents/src/stores/config.rs`), and precisely what `index_feed.rs`'s `covers()` preflight already enforces (an index must be rooted to contain the tree it serves). Multiple OKG stores (e.g., one agent with a personal tree and a Duetto tree) would be multiple named indexes, never one shared index disambiguated some other way — consistent with how `trusty-search` already treats multiple code repos.

## Decision

1. **`trusty-kb` is renamed `trusty-okg` and remains a native crate — not an MCP service, and not folded into `trusty-mcp-services` as a service of its own.** Its job, stated in the owner's own words: build and index open knowledge stores. The rename reverses ADR-0033's framing and confirms ADR-0040's reservation is now resolved: native.
2. **Indexing responsibility moves to `trusty-search`.** `trusty-search` gains OKF awareness — reserved-filename recognition, frontmatter-as-metadata parsing, and (eventually) `TrustLabel`-aware fencing at retrieval — and takes over the reconcile-and-push role `crates/trusty-agents/src/stores/index_feed.rs` plays today. This ADR does not design the mechanism (crawl-based vs. push-fed, which crate calls the ledger) — that is scoped follow-up.
3. **Each OKG store is a distinctly named `trusty-search` index**, mirroring the existing `trusty-search index <path> --name <name>` model. No shared/implicit index; one name, one root, one store.
4. **Agent-consumed reads are fronted by a new MCP service inside `trusty-mcp-services`, depending on `trusty-okg` as a plain path dependency — not by `trusty-okg` growing MCP tools of its own.** `kb_get_entity`, `kb_list`, `kb_list_trees`, `kb_status`, and `kb_convert_tree` cross into that service; `kb_ensure_structure` and `kb_validate` stay native-only; `kb_put_entity` is unresolved (see table above).
5. **The rationale is the owner's own consumer criterion (ADR-0040), applied twice.** Once to the capability (building/indexing a store is framework-consumed, hence native), once to the seam within it (reading/browsing/importing is agent-consumed, hence a thin MCP service in front of the native library). This makes both halves of the decision derivable rather than arbitrary, and establishes the split as a reusable pattern for future native capabilities that need partial agent access.

## Consequences

### Positive

- **Derivable, not arbitrary, on both halves.** The crate-level call and the read-path resolution both fall directly out of ADR-0040's consumer criterion, applied at two different granularities rather than as two unrelated judgment calls.
- **No new indexing surface to build for the wire layer.** `trusty-search`'s named-index model, register/reindex flow, and per-file `index-file`/`remove-file` routes already exist and already match what `index_feed.rs` calls — the wire-level move costs no new HTTP surface, only OKF-aware content handling on the ingestion side.
- **The read-path gap closes with a known, precedented shape.** The new OKG service inside `trusty-mcp-services` is structurally identical to how that crate already hosts `trusty-gworkspace` (ADR-0040) — a library dependency fronted by a service module — not a new packaging pattern to design from scratch.
- **Free rename.** `trusty-kb`'s CHANGELOG states explicitly: "This crate has not cut a release yet." ADR-0033 independently verified the same. No crates.io yank/deprecate cycle, unlike `trusty-gworkspace`'s already-yanked `0.1.0`.

### Negative / Trade-offs

- **This decision makes the "indexed by trusty-search" gap MORE load-bearing, not less.** The prior investigation found the canonical model's claim that every OKG store is "indexed by trusty-search" by default is not automatic today: it requires an explicit `[[stores]]` binding naming both `tree` and `index`, an agent-invoked ingest tool call (no CLI verb, no HTTP route, no GUI trigger), no scheduled runner despite DOC-63 §9 specifying one and the owner approving it 2026-08-03 (issue #4538, unbuilt — no `OkgRefresh`/`RefreshRunner`-shaped code exists anywhere in the workspace), and no file-watching. Moving indexing ownership INTO `trusty-search` does not close this gap by itself — if anything it raises the stakes of closing it, since `trusty-search` becoming the sole indexing path makes automatic/scheduled indexing the thing standing between "built" and "findable," rather than one contributing factor among several.
- **Rename touches a real, if bounded, surface.** 51 `trusty_kb::` call sites across 18 `.rs` files (all in `crates/trusty-kb` itself and `crates/trusty-agents/src/{stores,tools/okg}/*.rs`); 3 `Cargo.toml` entries (root workspace glob, `crates/trusty-agents/Cargo.toml:84`, `crates/trusty-kb/Cargo.toml` itself); 20 files mentioning "trusty-kb" in prose (ADRs 0022/0029/0033, DOC-55, DOC-63, `docs/specs/{README,trusty-agents-agents-sync}.md`, changelogs, a release-readiness doc, two research docs, `.claude-mpm/INSTRUCTIONS.md`). **A second naming surface exists and is not fully decided here:** the binary/tool names (`kb_status`, `kb_put_entity`, …) — this ADR places each of the eight tools (Decision point 4), but whether the surviving MCP-facing names stay `kb_*`-shaped or become `okg_*`-shaped once they live in `trusty-mcp-services` is unscoped.
- **`kb_put_entity`'s disposition is unresolved.** It is the one tool this ADR could not cleanly place — exposing it as-is in the new service would give agents a write path that bypasses the ledger/`TrustLabel` guarantees `okg_ingest_*` already enforces. Left as an explicit gap rather than force-fit into either side.
- **Indexing-ownership migration and the new service are both unscoped implementation work.** This ADR authorizes the direction — search absorbs OKF indexing, `trusty-mcp-services` hosts a new OKG-reads service — without designing either mechanism (crawl vs. push-fed; the service's tool schemas; how `TrustLabel` reaches a search chunk). Real follow-up, not designed here — the same posture ADR-0040 took toward its own `trusty-mcp-services` packaging particulars.

### Neutral

- **The name `trusty-okg` names the tree, while what the crate implements is the OKF container** — the crate builds OKF-formatted files that populate an OKG. This is a minor naming imprecision (OKF is the format, OKG is the built artifact), consistent with how the workspace already uses "OKG" colloquially for the whole system rather than reserving it strictly for the populated instance (DOC-63 itself: "the `okg` directory... an OKF store"). Not corrected here — `trusty-okg` is the owner's explicit naming choice, stated twice in the ruling ("keep the tool trusty-okg"), and this ADR records it rather than second-guessing it.
- **This does not reopen the `trusty-kb`-vs-`trusty-memory` redundancy question.** That was resolved by the owner on 2026-08-03 (DOC-63 §14 Q1): "keep both, separate jobs. The OKF store is knowledge built from sources; trusty-memory's KG is structured recall... no bridge." This ADR is silent on that axis by design.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-08-10:

- **ADR-0040 (`trusty-mcp` extracted from `trusty-common`; `trusty-mcp-services` absorbs `trusty-gworkspace` — the ADR written earlier today under a since-corrected number 0038): Amended by this ADR, on the one point it explicitly reserved, and extended by the new service.** ADR-0040's Decision states: "`trusty-kb`: not folded into `trusty-mcp-services`, not declared deprecated... Native-vs-MCP disposition is the owner's unmade call." This ADR makes that call — native for the capability — and additionally gives `trusty-mcp-services` a second resident service (alongside `trusty-gworkspace`) for the agent-consumed slice, using the identical library-dependency shape ADR-0040 already established. Everything else in ADR-0040 (the `trusty-mcp`/`trusty-mcp-services` two-crate extraction, the `trusty-channels` correction, the consumer-criterion rule itself, the `inventory`-based service registry, the WASM deferral) is untouched. The consumer criterion ADR-0040 stated is not amended — it is applied twice.
- **ADR-0033 (`trusty-mcp` consolidates native MCP services; Superseded by 0040): Consistent with its supersession, and further distanced.** ADR-0033 proposed folding `trusty-kb` into an MCP-services crate wholesale, on an obsolescence premise ADR-0040 already found unfounded. This ADR goes further in the opposite direction from ADR-0033's original proposal on the crate itself — "keep it native, explicitly, by owner ruling" — while still landing an agent-facing slice inside the same `trusty-mcp-services` crate ADR-0033 first proposed, via the two-layer split rather than ADR-0033's flat fold-in. No conflict: ADR-0033 is already superseded and this ADR does not revive any part of its specifics.
- **ADR-0038 (KG stays additive in recall, gated on extraction quality; Accepted, `origin/main`): No interaction — different subject under a colliding number.** This ADR does not reference or rely on "ADR-0038" for any content; every citation of the trusty-mcp-services decision in this document names it as **ADR-0040**, precisely to avoid the collision this repo hit twice in one day.
- **ADR-0022 (Knowledge-tree sync model: config-only monorepo + separate per-store repos; Accepted): Consistent, no interaction.** ADR-0022 governs how OKG tree *content* syncs across machines (per-store git repos, opt-in via `sync_remote`) — a storage/sync question. This ADR governs which crate builds/indexes that content, which crate serves agent reads over it, and whether either is MCP or native — a packaging/consumer-model question. Orthogonal axes; the rename to `trusty-okg` does not touch ADR-0022's `bobmatnyc/trusty-kb-<tree>` naming convention for per-store sync repos, which is now stale prose (references a crate name this ADR retires) but not a substantive conflict — flagged as a documentation follow-up, not a decision conflict.
- **DOC-55 (Universal OKG Importer, Draft) / DOC-63 (OKG Sources, Accepted): Consistent, and this ADR's Context relies on both.** Neither spec takes a position on MCP-vs-native for the crate or its read surface; both describe the ingestion/ledger/trust mechanics this ADR's Context cites (`SourceItem`, `TrustLabel`, the index backlog/journal) without prescribing where indexing or reads happen. DOC-63 §9's scheduled-refresh design (unbuilt, #4538) becomes more load-bearing under this ADR's Decision, per Consequences above — DOC-63 itself is not amended, since its runner design says nothing about which crate performs the eventual push or which crate serves reads.
- **DOC-63 §14 Q1 (trusty-memory KG vs. OKG, owner-resolved 2026-08-03): Consistent, explicitly not reopened.** See Consequences, Neutral.

No conflict with any other Accepted or Proposed ADR.

## References

- Owner ruling, verbatim, 2026-08-10, both parts (this ADR's Context)
- `docs/adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md` — amended by this ADR (the `trusty-mcp-services`/`trusty-gworkspace` ADR, renumbered from its original same-day 0038 to avoid a collision with the unrelated `origin/main` ADR now holding that number)
- `docs/adr/0033-trusty-mcp-consolidates-native-mcp-services-into-one-crate.md` — superseded by 0040, further distanced by this ADR
- `docs/adr/0022-knowledge-tree-sync-model.md`
- `docs/specs/okg-universal-importer.md` (DOC-55)
- `docs/specs/DOC-63-okg-sources.md`, especially §14 Q1
- `crates/trusty-agents/src/stores/index_feed.rs` — the indexing logic this decision moves out of `trusty-agents`
- `crates/trusty-agents/src/stores/binding.rs` — `okg_tree_path`/`bound_index_for_tree`, the tree<->index resolution this ADR's "named and indexed distinctly" section builds on
- `crates/trusty-kb/src/okg/{trust,index_journal}.rs` — `TrustLabel`, the ledger/journal reconcile logic
- `crates/trusty-kb/src/schema.rs` — OKF v0.1 container rules, reserved filenames
- `crates/trusty-kb/src/tooldefs.rs` — the eight `kb_*` tool definitions this ADR splits across native and service
- `crates/trusty-search/src/commands/index.rs`, `src/main.rs:269` — the named-index model (`trusty-search index <path> --name <name>`)
- `crates/trusty-search/src/mcp/tools/index.rs:68,86` — `index-file`/`remove-file`, the routes `index_feed.rs` already calls
- `crates/trusty-kb/CHANGELOG.md` — "This crate has not cut a release yet"
