# Slice plan: epic #10746, build the trusty-okf crate

Spec: SPEC-OKF-01 r7.1 (§6.2 criteria, numbered 1-147; criterion 41 withdrawn). Criteria 125-147 checked individually 2026-11-04. Decisions: ADR-0075, ADR-OKF-0001. Crate: `crates/trusty-okf` in trusty-tools, `publish = false`. Code starts only after wave 2 is done and this plan is approved. Status 2026-11-23: wave 2 is done (all #10737 sub-issues have merged PRs) and the slice order is approved. Slices S3 to S13 have branches. Each slice is one PR. Every slice runs the S1 boundary check.

Wave-2 branches: D1 #10738 (search index identity), D2 #10739 (path admission), D3 #10740 (binding excludes), #10741 (DOC-55), #10742 (DOC-63), #10743 (ADR-0075), #10744 (ADR-0022), #10745 (worker dir, MSRV).

## S1. Crate scaffold and boundary guard
- Scope: empty crate with the §4.1 module tree and feature flags `search-feed`, `extract-worker`; a script that checks B1-B10.
- True: crate is a workspace member; `cargo tree` shows none of the banned deps; no `env::var`, `okg`, `assistant-okg-` literals in `src/`; core builds with `--no-default-features`.
- Deps: #10743, #10745. Risk: Low.
- Test: the boundary script; criteria 11-17; fails when a banned dep is added.

## S2. Format, profile, validate
- Scope: `git mv` of frontmatter, entity, schema, validate, reconcile. v0.2 fields, trust tier, staleness, raw-preserving round trip.
- True: criteria 1-10, 18-22, 113-115. Unknown keys and flow-style comments survive. Strict write refuses a missing `type` and changes no file.
- Deps: S1. Risk: Normal.
- Test: parse-render-parse over the four Google-spec bundles at `ad30107c`; the §2.2.1 fixtures.

## S3. Bundle core and path confinement
- Scope: `Bundle` (open, put, move), `index.md` regenerator, `bundle::paths`, graph read, verification invalidation (§2.10).
- True: criteria 7, 8, 31-36, 52-57, 125, 140, 146. A second `regenerate_indexes` writes zero bytes. Hand-written `index.md` stays byte-identical. Symlink and `..` escapes are refused.
- Deps: S2. Risk: Normal.
- Test: idempotence test; path-escape fixtures.

## S4. Build layer move
- Scope: registry, ledger, jsonl, ingest, docstore, policy, trust, `Locator::Connector` variant.
- True: criteria 116-117. Existing `registry.toml` rows (including Gmail, Drive) still load. Ledger on-disk format is unchanged. Re-ingest keeps editorial and verification keys. Trust labels fail closed. DOC-63 sent-only scope (§3.3) is honoured.
- Deps: S3, #10742. Risk: High (registry compatibility).
- Test: golden `registry.toml` and ledger fixtures from trusty-kb at `c1eed98441`.

## S5. State separation and crash recovery
- Scope: `state` module, revision journal, machine-local index-state directory, operation journal (§4.11).
- True: criteria 60-70, 106-109, 118-119, 126. Index state never lives in the bundle. No state reads Searchable without D1. A crash at each named step boundary recovers to a consistent bundle.
- Deps: S3. Risk: High.
- Test: fault injection at every step boundary; a bundle copied to another machine carries no index state.

## S6. Corrections, migration, restore
- Scope: supersede, deprecate, tombstone; lossless trusty-kb to v0.2 migrate; `adopt` (3q C); rebuild history.
- True: criteria 23-30, 37-40, 42, 110-112, 120, 127, 130, 131, 142, 143. Migrated bundle restores byte-for-byte. Source preservation holds.
- Deps: S4, S5. Risk: Normal.
- Test: migrate, restore and diff on a trusty-kb fixture tree.

## S7. Feed and retrieval (`search-feed`)
- Scope: move `index_feed.rs` and `index_feed_rpc.rs`; reconciliation F1-F6; `retrieve`; scope check.
- True: criteria 43-51, 128, 133, 134. The feed counts a put only when `content_sha256` matches. A scope leak returns `IndexScope`. The probe is the only source of Searchable.
- Deps: S5; D1 #10738, D2 #10739, D3 #10740 merged in trusty-tools (or fakes for the fallback paths).
- Risk: High.
- Test: scripted fake daemon, with and without D1; one live-daemon test for F6 and criterion 128.

## S8. Import engine and document-tree import
- Scope: `import` (plan, apply, ID rule, report), `extract` built-in tier, `docstore` adapter, CLI.
- True: criteria 71-85, 58-59, 129, 135-138, 141, 144. Re-import is idempotent. Protected keys are not overwritten.
- Deps: S4, S6, S7 (criteria 85, 129, 135 and 141 need the feed). Risk: Normal.
- Test: plan, import, re-import on a fixture tree; report diff is empty; one live-daemon test for criterion 141.

## S9. Email and Google Docs adapter contract
- Scope: adapter spawn API, normalisation, provenance, Appendix C matrix. No connector code.
- True: criteria 86-98, 132. B10 holds: the adapter gets exactly the caller's environment list; trusty-okf adds none.
- Deps: S8, #10741. Risk: Normal.
- Test: fake adapter processes; environment-capture test.

## S10. Extraction worker (`extract-worker`)
- Scope: worker client, Appendix B protocol, deadlines.
- True: criteria 99-105 (client half; S10b owns the criterion 104 worker half and the criterion 105 Appendix B fixtures), 121-124, 145, 147. Worker starts with a cleared environment plus allow-list and no network. `cargo tree --all-features` shows no xberg, ort or candle-core.
- Deps: S8, #10741, #10745. Risk: High.
- Test: fake worker for crash, hang and oversize output; each test runs under a timer equal to the deadlines plus 5 s.

## S10b. okf-extract worker binary
- Scope: the xberg worker binary in `workers/okf-extract`, speaking the Appendix B protocol to the S10 client. Added 2026-11-04 (option B); criteria 99-105 are unchanged, only the owner of these two halves moved.
- True: criterion 104 worker half (`cargo tree -e features` on the worker binary); criterion 105 Appendix B fixtures (PDF, DOCX, XLSX, PPTX, zip bomb inside the memory and time limits, text PDF extracted twice to byte-identical output).
- Deps: S10; xberg 1.4.0 available to the build. Dispatch waits until it is settled how xberg 1.4.0 is obtained. Risk: High.
- Test: the Appendix B fixtures run against the real worker through the S10 client.

## S11. trusty-agents cut-over and trusty-kb removal
- Scope: switch the 57 `trusty_kb::` call sites; move `Roots` mapping and `gmail`/`drive` kinds into trusty-agents; delete `crates/trusty-kb`.
- True: criterion 139 (adapter half from S9, worker half from S10); the agent suite is green; `cargo tree -i trusty-okf` lists trusty-agents; no dependency edge runs from trusty-okf to trusty-agents.
- Deps: S4-S10. Risk: High.
- Test: existing trusty-agents okg and store tests unchanged; workspace build without `trusty-kb`.

## S12. trusty-agents index feed onto trusty-okf
- Scope: move trusty-agents off its in-tree `_sources/<id>.index.jsonl` journal (`crates/trusty-agents/src/kb/okg/index_journal.rs`) and its own `stores/index_feed*.rs` onto `trusty_okf::feed` and `trusty_okf::state::index`, with machine-local index state (SPEC §4.9.1). Include a journal migration from the in-tree files. Added 2026-11-04.
- True: SPEC §2.7.2 rows for index_journal and index_feed hold for trusty-agents. No synced tree carries another machine's Acknowledged or Searchable state (the r4 BLOCKER-5 defect closes). Existing journals migrate without losing acknowledged entries.
- Deps: S11. Risk: High.
- Test: migration of a fixture journal; two machines sharing a synced tree keep separate index state.
- Until S12 lands, epic closure claims neither §4.9.1 nor option 3e A for trusty-agents.

## S13. Connector adapter binaries (proposed)
- Scope: the Gmail and Google Docs/Drive adapter executables that SPEC §3 (m) A expects trusty-agents or a connector crate to ship. They speak the S9 adapter protocol (Appendix C) and hold the credentials. trusty-okf adds none (B10). Built in trusty-agents, or in a new connector crate outside trusty-okf.
- True: the trusty-okf CLI imports a real mailbox and a real Docs folder through these binaries. Criterion 98's `in:sent` half holds in the adapter. Criterion 144's `child_processes` setting is exercised for real.
- Deps: S9, S11. Risk: High.
- Test: a live-lane import against a test account; the adapter conformance tests reuse the S9 fake-adapter suite.

## Order and parallelism
S1 -> S2 -> S3 -> (S4, S5) -> (S6, S7) -> S8 -> (S9, S10) -> S11 -> (S12, S13). S10b (worker binary) runs after S10 once the xberg pin is settled; it blocks no other slice. S6 and S7 run in parallel after S5; S6 also needs S4. S8 starts after S6 and S7. #10744 (ADR-0022 sync rule) is docs-only and blocks no slice. Each slice also closes the §6.2 criteria it names; epic closure needs all of §6.2.
