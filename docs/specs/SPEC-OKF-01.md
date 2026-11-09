# SPEC-OKF-01 — trusty-okf: building and maintaining OKF bundles

**Status:** Accepted r7.1 — owner accepted as written, 2026-11-03 09:16Z (Bob). Pre-implementation filings in progress; no implementation yet.
**Date:** 2026-11-03
**Author:** trusty-mpm documentation agent, from the Architect's brief (goal ar-G01)
**Owner:** Bob
**Spec ID:** `SPEC-OKF-01~draft`
**DOC-N claim:** `DOC-89`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). No other `DOC-89` on `origin/main` at 2026-11-09. Renumbered from `DOC-87`, which `DOC-87-typed-owner-constraints.md` already held (#10743, review of #10811).
**Related:** ADR-0022, ADR-0024, ADR-0026, ADR-0029, ADR-0040, ADR-0041, DOC-45, DOC-55, DOC-63 (all in `bobmatnyc/trusty-tools`, `docs/adr/` and `docs/specs/`); the Xberg assessment (ar-T05, `docs/briefs/2026-11-03-xberg-assessment.md` in trusty-tools worktree `f93609dd`); proposal [ADR-OKF-0001](../adr/0075-trusty-okf-consolidates-trusty-kb-and-owns-the-index-feed-loop.md) (filed as ADR-0075)
**Evidence read at:** trusty-tools `c1eed98441`; OKF spec repo `ad30107c31c06aec8a7d5636e0d1058118604e6f`; wmahfoudh/okf `7d6286ba112177394f08bf7b79e56f54a1fd22ec`
**Filed in trusty-tools:** 2026-11-03 under [#10743](https://github.com/bobmatnyc/trusty-tools/issues/10743), unchanged except for this line and the ADR-OKF-0001 links, which now point at [ADR-0075](../adr/0075-trusty-okf-consolidates-trusty-kb-and-owns-the-index-feed-loop.md). ADR-0075 also records the §3 (e) feed-loop amendment that §6.1 item 9 names. Paths below such as `docs/specs/SPEC-OKF-01-review.md` refer to the trusty-okf project, which keeps the review, the acceptance reconciliation, the owner-choices list and the readback beside its copy of this spec; they are not copied here.

This spec proposes what the trusty-okf crate does. Bob decided §3 (a) on 2026-11-03. Bob also approved four inputs on 2026-11-03: state separation, document-tree import, email and Google Docs import, and the Xberg extraction worker (§1). On 2026-11-03 at 06:58Z Bob decided every remaining question in §3 and §7, and every other owner choice, as recommended (§1.5). No code exists yet.

**Amendments of 2026-11-05 (owner rulings, Bob; #10746).** D14 drops `convert` from §2.7.2 and §4.1. D19 pins the extraction worker to `xberg =1.3.10` in §2.16. D4b amends §2.7.2 for the local constants slice S4 uses and records that the advisory lock (§4.4) is built in slice S5. Each change carries an inline `(Amended 2026-11-05, …)` note.

**Revision r7.1 (2026-11-03).** Applies the four fixes and two MINORs of the delta review of r7 (`docs/specs/SPEC-OKF-01-review.md`, "Delta review of r7"). No owner decision changes.

1. `adopt` takes `(ConceptId, ByteHash)` pairs and refuses a concept whose bytes changed since the review. It needs a `Human` actor with explicit confirmation, and its refusal reasons are named ([§4.5](#SPEC-OKF-01-4-5) "Adopt"; §4.2; §4.3; criterion 142).
2. The OS sandbox check rests on the host's own spawn evidence. The worker's `hello` `sandbox` field is a cross-check, and a mismatch is `worker_incompatible` ([§2.16](#SPEC-OKF-01-2-16); Appendix B; criterion 145).
3. The `acquire` request carries `operator_initiated`. When the field is absent, the adapter applies SENT-only ([§2.15](#SPEC-OKF-01-2-15); Appendix B).
4. D3 reads "the binding admits every path except `_sources/**` and `_state/**`", stated as literal `exclude_globs` ([§5.3.1](#SPEC-OKF-01-5-3-1) rule 1; [§5.6](#SPEC-OKF-01-5-6) D3; §6.1 item 11).
5. Criterion 144 tests exit code 2 through the library's error-to-exit-code mapping.
6. `Verification` gains an optional `stale_after`, so a `verify` can extend `stale_after` in the same operation (§2.4; §4.2; criterion 146).

**Revision r7 (2026-11-03).** Records the owner ruling of 06:58Z, "all recommended", on the 26 items of `docs/specs/SPEC-OKF-01-owner-choices.md` (§1.5).

1. Each §3 question (b)–(r), sub-question (c2) and §7 reads "Decided 2026-11-03 (owner)". Rejected options stay visible, marked "Not chosen" or "Ruled out by r6".
2. Every "depends on 3x" passage now states the decided behaviour.
3. The eight decisions without a §3 question are recorded where they apply: `verified[]` on a content change (§2.10), `stale_after` extension (§2.4), extraction provenance in the content hash (§2.10), stale-revision hits (§5.2), the worker OS sandbox (§2.16), native-Doc export review (§2.15), sub-agent imports and ADR-0024 (§2.12, §2.15), and the acceptance scope (§6.1 item 3).
4. Items 19–26 of the owner-choices list are decided as recommended and stay revisitable at implementation. A change to one of them does not reopen acceptance (§1.5, §6.1 item 3).
5. D2 and D3 follow the delta review of r6 (`docs/specs/SPEC-OKF-01-review.md`, "Delta review of r6"). D2 adds `include_paths` and `path_filter` to `search.index.config.get`, or a per-path admission query. D3 also requires the binding to admit every concept path (§5.3.1, §5.6).
6. §6.1 marks which acceptance items are satisfied and adds the pre-implementation filings that the decisions need (items 13–16).
7. New criteria 144–147 test the decisions that r6 had no criterion for. Criterion 41 is withdrawn, because 3c B was not chosen.

The delta review also suggests treating D1 as a release gate for `--feed`. No owner-choice item covers that, so r7 does not adopt it.

**Revision r6 (2026-11-03).** Resolves the 12 rows that `docs/specs/SPEC-OKF-01-acceptance-reconciliation.md` classes BLOCKS ACCEPTANCE. Each fix states what must be true and adds criteria 128–143 (§6.2, "Acceptance-reconciliation fixes (r6)"). r6 decides no §3 question and does not decide §7. Where a fix constrains an open option, the text says so. r6 adds three external dependencies, D1–D3 (§5.6). The OWNER CHOICE and RECORDED rows are unchanged.

| Row | Item | Section | Criteria |
|---|---|---|---|
| 1 | No `_sources/` or `_state/` file in the search index | [§5.3.1](#SPEC-OKF-01-5-3-1); §4.11 "Staged names"; §5.4 F16; §5.5; [§5.6](#SPEC-OKF-01-5-6) D2, D3 | 128, 129 |
| 2 | trusty-okf's own writes are not hand edits | [§4.5](#SPEC-OKF-01-4-5) "Ownership proof" | 130, 131 |
| 3 | A truncated listing never drives deletion | [§2.13](#SPEC-OKF-01-2-13) "Listing completeness"; §2.15 items 2, 8; Appendix A | 132 |
| 4 | The probe needs evidence specific to the current bytes | [§4.9](#SPEC-OKF-01-4-9) "The retrieval probe" | 133 |
| 5 | Index state is bound to an index identity | [§4.9.2](#SPEC-OKF-01-4-9-2); §4.6; §5.4 F14–F15; [§5.6](#SPEC-OKF-01-5-6) D1 | 134, 135 |
| 6 | The engine addresses concepts by full ID | [§2.13](#SPEC-OKF-01-2-13) "Full-ID addressing" | 136 |
| 7 | One source file yields one concept | [§2.14](#SPEC-OKF-01-2-14) rule 3 | 137 |
| 8 | `build`, `rebuild` and `import` refuse path collisions | [§2.9](#SPEC-OKF-01-2-9) "Path collisions"; §2.14 rule 4 | 138 |
| 11 | B10 binds the worker; adapters get a caller-supplied environment | [§2.7.1](#SPEC-OKF-01-2-7-1) B10; §2.16 | 139 |
| 13 | Case and Unicode fold collisions are refused | [§2.11](#SPEC-OKF-01-2-11) "Fold rule" | 140 |
| 14 | A tombstone's frontmatter stays within 64 KiB | [§4.8](#SPEC-OKF-01-4-8) "Tombstone detection contract"; §2.10 | 141 |
| 17 | An event with no hash is history | [§2.10](#SPEC-OKF-01-2-10) "Events with no hash"; §4.9 Axis 3 | 142, 143 |

**Revision r5.1 (2026-11-03).** Fixes N2 from the delta review of r5 (Architect ruling 04:10Z): the pre-image check does not apply during roll back or `Bundle::recover`, and `recover` clears its conflict marker (§4.11; criterion 127).

**Revision r5 (2026-11-03).** Closes the two open items of the delta review of r4 (`docs/specs/SPEC-OKF-01-review.md`, "Delta review of r4"). MAJOR findings and open decisions are unchanged.

1. A malformed `verified` or `generated` value that an operation owns moves to `legacy_verified` or `legacy_generated` with a warning, never a silent drop ([§2.2.1](#SPEC-OKF-01-2-2-1); §2.10). Criterion 113 is restated, and criterion 125 is new.
2. Each file step checks its pre-image hash before it writes, in normal execution and in recovery. A mismatch stops the operation as `recovery_conflict` ([§4.11](#SPEC-OKF-01-4-11); §4.5). Criterion 112 now covers `log.md`, and criterion 126 is new.
3. §6.1 gains an item that files the sync rule against ADR-0022 / DOC-56 Phase 2.
4. Criteria 36 and 64 agree on `trust_tier`: the OKF tier comes from raw frontmatter, and the verification state reports the out-of-band edit (§4.9; §2.10).
5. Criteria 106 and 108 name the compared file set. Criterion 124 names the start of the reap deadline.

**Revision r4 (2026-11-03).** Fixes the seven BLOCKERs of the r3 review (`docs/specs/SPEC-OKF-01-review.md`). The review's MAJOR and MINOR findings stay open as recorded comments. Each fix states an invariant and adds acceptance criteria 106–124 (§6.2).

1. Multi-file crash recovery: an operation journal with intent and completion records, recovered on the next open ([§4.11](#SPEC-OKF-01-4-11); §2.9).
2. `rebuild` keeps every `log.md` entry, proves ownership before it overwrites, and swaps from a staging area ([§4.5](#SPEC-OKF-01-4-5)).
3. Raw-preserving round trip: raw frontmatter is the source of truth, and typed fields are views ([§2.2.1](#SPEC-OKF-01-2-2-1); §4.2).
4. Imported data cannot set verification or lifecycle keys ([§2.13.1](#SPEC-OKF-01-2-13-1); §2.14 rule 11).
5. Index state is machine-local ([§4.9.1](#SPEC-OKF-01-4-9-1); §3 (r)).
6. The migration manifest keeps a full backup of each changed file ([§2.9](#SPEC-OKF-01-2-9)).
7. Worker handshake and shutdown have deadlines ([§2.16](#SPEC-OKF-01-2-16); Appendix B).

r4 adds open questions §3 (q) and (r).

**Revision r3 (2026-11-03).** Records the four owner inputs as separate §1 entries. Adds the state separation model (§4.9), which replaces r2's single "Synced" claim (§4.6, §5.4). Adds import (§2.13–§2.16, §4.10): one common import interface, document-tree import, email and Google Docs import, and the optional extraction worker. Adds open questions §3 (k)–(p). Adds acceptance criteria §6.2 items 60–105. Moves detail tables into Appendices A–C.

**Revision r2 (2026-11-03).** Records the §3 (a) ruling. Adds the consolidation mapping and the agent-glue boundary rule (§2.7). Adds v0.2 field mapping (§2.8), lossless migration (§2.9), verification invalidation (§2.10), path confinement (§2.11), ADR boundaries (§2.12), the correction and tombstone lifecycle (§4.8) and index reconciliation failures (§5.4). Rewrites the acceptance criteria (§6).

**Reading rule.** Text marked "Decided 2026-11-03 (owner)" records an owner pick (§1.5). r7 replaces every r6 "depends on 3x" marker with the decided behaviour. Text marked "proposal" is a design proposal that Bob has not ruled on. Text marked "inference" is a conclusion the cited evidence does not state directly.

## Glossary

Each term below means one thing in this spec.

| Term | Meaning |
|---|---|
| **OKF** | Open Knowledge Format, version 0.2, as written in the Google spec repo's `SPEC.md`. |
| **bundle** | One OKF directory tree: concepts, plus optional `index.md` and `log.md` files ([SPEC.md §3](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/SPEC.md#L109)). |
| **bundle root** | The absolute directory path of one bundle. The caller passes it to trusty-okf. |
| **concept** | One non-reserved `.md` file in a bundle. Its ID is its bundle-relative path without `.md` (§2). |
| **store** | A document source registered in a bundle's registry. It is either a pointer to a local directory or an extract from a connected system. For an agent's OKG this is DOC-63's store. |
| **OKG** | The one canonical bundle each agent has. trusty-okf builds it from that agent's stores, and trusty-search indexes it. |
| **agent glue** | Code that knows the trusty-agents home layout, agent names or slugs, agent configuration, credentials, connectors or fence rendering. |
| **content hash** | The hash that decides whether a concept's content changed (§2.10). |
| **verification record** | The retained list of every verification event for a concept, including invalidated ones (§2.10). |
| **revision** | One content state of a concept, named by its content hash. A write that keeps the content hash keeps the revision (§4.9). |
| **save state**, **index state**, **verification state** | The three independent states of a concept's current revision (§4.9). |
| **current-revision verification** | The verification state: whether a verification event is bound to the current revision's content hash. It is distinct from the OKF trust tier (§4.9). |
| **import** | One run that reads one store and writes concepts into one bundle (§2.13). A store is the import's input; the bundle is its output. |
| **source adapter** | Code that lists and fetches items from one kind of store. It is DOC-55's `Connector` (§2.13). An authenticated source adapter holds credentials; trusty-okf has none. |
| **extractor** | Code that turns bytes plus a type hint into text and fields. It is DOC-55's `Extractor` (§2.13). |
| **extraction worker** | The optional separate process that runs xberg. trusty-okf calls it through the worker protocol (§2.16). |

Supporting terms. A **search index** is a named trusty-search index; `index.md` always means the file. The **ledger** is the build state under `_sources/`: the registry and the per-store item ledger. The **index journal** records what this machine's search daemon confirmed; r4 moves it out of the bundle (§4.9.1). The **operation journal** records each multi-file operation before its first write (§4.11). The **feed** is the loop that pushes concepts to a search index and withdraws them. A **trust label** is Trusty's per-store label (`UntrustedExternal` or `UserAuthored`). An **OKF trust tier** is OKF's derived tier (unverified, machine-confirmed, human-reviewed).

Note on usage. trusty-kb and ADR-0041 call the OKG tree itself a "store" (`KbStore`). This spec does not. Here a store is always a source.

## 1. Confirmed requirements {#SPEC-OKF-01-1}

From Bob, 2026-11-03:

1. trusty-okf is a separate project inside Trusty. It is a separate Rust crate named `trusty-okf`. It runs in its own session.
2. trusty-okf owns building and maintaining OKF repositories: the repository and content lifecycle.
3. trusty-search is the search engine. trusty-okf calls it through its retrieval interface.
4. The first target is knowledge repositories for Trusty agents and assistants.
5. Wider document stores and repos come later, as an investigation only. They are out of first scope.
6. "Move OKF into a separate crate." This spec finds the existing OKF code in trusty-tools and says where each part goes (§2.7).

Owner ruling, 2026-11-03 03:06Z (§3 (a), option A):

7. Generic trusty-kb functionality consolidates under trusty-okf.
8. trusty-okf supersedes the planned `trusty-okg` crate name from ADR-0041.
9. The agent-glue boundary stays. Agent-specific and assistant-specific code stays in trusty-agents.

This ruling decides only §3 (a). It authorizes design and review, not implementation or installation.

### 1.1 Owner requirement, 2026-11-03 03:11Z: state separation (approved MUST-HAVE) {#SPEC-OKF-01-1-1}

Bob approved this as a must-have before implementation starts. It is separate from the §3 (a) ruling.

**Approved.**

10. trusty-okf tracks three states independently for the current content revision of each concept: saved, indexed/searchable, and verified.
11. Save succeeds and index fails: the state is saved and pending-index. trusty-okf never reports it as synced.
12. Content changes: prior verification stays as history. The current revision is unverified until verification evidence matches it.
13. An index acknowledgement alone is not proof of retrieval.
14. The canonical OKF advisory trust tier may stay distinct from current-revision verification.

Bob's confirmation, in substance: failed search updates and historical reviews must not imply current verification or "done".

**Proposals under this input (not owner-approved).** The state model, its storage, the retrieval probe and the operation table in §4.9.

### 1.2 Owner requirement, 2026-11-03 03:13Z: document-tree import (approved as scope) {#SPEC-OKF-01-1-2}

Bob: "Let's also include an import function to import document trees into OKF."

**Approved.**

15. trusty-okf has an import function that imports a document tree into an OKF bundle.
16. It has an import API and an import CLI.
17. It reuses DOC-55 instead of duplicating it. (Amended 2026-11-05, #10746 D14: `convert.rs` is not reused; it has no caller.)
18. The import expands first scope to existing document trees. It does not make trusty-okf a general document-store or search service.

**Proposals under this input (not owner-approved).** Every detail in §2.14 and §4.10: hierarchy and provenance preservation, an unchanged source tree, preview, deterministic IDs and collisions, repeat-import behaviour, unsupported-format reports, exclusions and symlink confinement, interrupted-import recovery, no automatic human verification, the report categories and the adapter boundary.

### 1.3 Owner requirement, 2026-11-03 03:15Z: email and Google Docs import (approved as required scope) {#SPEC-OKF-01-1-3}

Bob rejected limiting email ingestion to prepared summaries: "No, let's include email, Google Docs, common ways of document importation."

**Approved.**

19. Direct email import and direct Google Docs import sit alongside document-tree import.
20. One extensible common import interface serves all three.
21. Real source ingestion is required scope.
22. trusty-okf owns normalization, provenance and the repository lifecycle. Source-specific authenticated acquisition may stay in source adapters or agents.
23. The public import API and CLI support these sources end to end.
24. Saved, indexed and current-revision verified stay distinct for imported content.
25. The default never silently forces summaries-only. The default never copies whole raw mailboxes.
26. The content policy is configurable. Its default is an open owner choice (§3 (k)). r7: decided 2026-11-03, §3 (k) option A (§1.5).

This is design authorization only. It grants no account access, no bulk import, no implementation and no publishing.

**Proposals under this input (not owner-approved).** The format and platform matrix (Appendix C) and the proposed acceptance in §2.15: explicit account and source selection, complete pagination, immutable source IDs, revisions and dates, incremental import and dedup, email thread context, Docs structure and export fidelity, reported format failures and deletions, and resumability.

**Constraints that still hold.** The new scope does not lift these rulings.

- A credential grant does not survive delegation. A sub-agent must hold its own grant (owner 2026-08-01; ADR-0026, `docs/adr/0026-credential-grants-do-not-survive-delegation.md:1-7@c1eed98441`; DOC-45 `SPEC-CREDAUTH-04`, `docs/specs/DOC-45-credential-authority-model.md:378-386@c1eed98441`).
- Trust labels and chunk-level fencing land before the connector grants [#4546](https://github.com/bobmatnyc/trusty-tools/issues/4546)–[#4549](https://github.com/bobmatnyc/trusty-tools/issues/4549) (owner 2026-08-01; DOC-63 Q2 resolution, `docs/specs/DOC-63-okg-sources.md:1655-1660@c1eed98441`).
- Gmail import is SENT-only in the prior OKG source spec (owner 2026-08-01, as relayed by the PM). DOC-63 states it as normative S-3.3: the constraint sits in the connector's `list`, at the query (`docs/specs/DOC-63-okg-sources.md:601`, `:643-648@c1eed98441`). DOC-63 records it as spec text; this spec found no separate owner quote for it.

§3 (l) and §3 (m) recorded where the new scope seemed to conflict with these rulings. The 06:58Z ruling settles both (§1.5). SENT-only now binds the automated OKG refresh only (§3 (l)). A sub-agent gets the in-process tier only, so ADR-0024 stays unchanged (§3 (m)). The other two constraints stand as written.

### 1.4 Owner design choice, 2026-11-03 03:17Z: Xberg as a separate worker (approved) {#SPEC-OKF-01-1-4}

Bob, after the separate-worker recommendation: "Yeah, let's do that."

**Approved.**

27. Import uses an optional, separate extraction worker.
28. The worker depends on a pinned xberg crate.
29. trusty-okf import calls the worker through a versioned process protocol.

This approves the design choice only. It does not accept the whole spec, and it does not authorize bulk import of accounts.

**Proposals under this input (not owner-approved).** Every detail in §2.16 and Appendix B: limits, cancellation and reaping, local-only configuration, provenance and errors, the handshake, absent-worker behaviour and fixtures. Process isolation alone is not a permission sandbox (Xberg assessment, `docs/briefs/2026-11-03-xberg-assessment.md:269`).

### 1.5 Owner ruling 2026-11-03 06:58Z: all recommended {#SPEC-OKF-01-1-5}

Bob, via the dashboard: "All recommended", on all 26 items in `docs/specs/SPEC-OKF-01-owner-choices.md`. Each item resolves to its "Recommended" option. This entry is separate from the rulings in §1 and §1.1–§1.4.

Scope of the ruling (item 1 A). Items 1–18 are decided for acceptance. Items 19–26 are also decided as recommended. Each of items 19–26 is revisitable at implementation, and a change to one of them does not reopen acceptance. The ruling decides design. It grants no account access, no credential grant, no bulk import and no implementation start.

| Item | Reference in this spec | Decision |
|---|---|---|
| 1 | §6.1 item 3 | A. Items 1–18 decide acceptance. Items 19–26 are decided (recommended), revisitable at implementation. |
| 2 | §3 (l); §2.15 "Inbound email" | C. SENT-only binds the automated OKG refresh only. An explicit operator-initiated inbound import is allowed only after the capability-grant review, labelled `UntrustedExternal`. |
| 3 | §3 (m) note; §2.12 ADR-0024; §2.15 | A. A sub-agent gets the in-process tier only (text and Markdown). ADR-0024 stays unchanged. |
| 4 | §3 (m) | A. Separate adapter programs, one per source kind, started by the trusty-okf CLI with a caller-supplied environment list. |
| 5 | §3 (d); §2.10 | C. The verification record outside frontmatter is authoritative. Frontmatter holds a bounded copy. |
| 6 | §3 (q); §4.5 | C. Equality proof first, then `adopt` on a reviewed list. No legacy event gains a hash. |
| 7 | §3 (r); §4.9.1 | A. The caller passes an index state directory outside the synced store tree. |
| 8 | §2.10 "What a content change does" | A. A content change removes OKF `verified` entries. The old review stays as history. This is a trusty policy. |
| 9 | §2.16 "OS sandbox" | B. The worker runs under an OS sandbox before it parses email or Drive content. Local-tree extraction may ship before it. |
| 10 | §2.15 "Native-Doc export"; Appendix C | B. Native Google Doc export goes through the capability-grant review. |
| 11 | §3 (c2); §4.5 | (i), listed as A. The import writes the new source content and keeps the curator's keys. |
| 12 | §3 (c); §2.5; §4.8 | A. Deprecate in place with `superseded_by` and `supersedes`, recorded in `log.md`. |
| 13 | §3 (b); §2.8; §2.9 | B. Write v0.2. Read v0.1. Migrate an OKG by rebuilding it. |
| 14 | §3 (e); §2.6 | A. trusty-okf owns the feed loop, in the feature-gated module `trusty_okf::feed`. |
| 15 | §3 (g); §4.10 | B. Library plus a CLI (`validate`, `regen`, `build`, `migrate`, `feed`, `import`). |
| 16 | §3 (i); §5.4 F9 | A. Withdraw deprecated and tombstoned concepts from the index, and re-check on every feed pass. |
| 17 | §3 (k); §2.15 | A. Full body text with thread context. Quoted text kept and marked. Attachments listed, not extracted. |
| 18 | §3 (j); §7 | A. `crates/trusty-okf` in the trusty-tools workspace. |
| 19 | §2.4 "Extending `stale_after`" | B. Only a person or a fresh `verify` extends `stale_after` on a human-reviewed concept. Any writer may shorten it. Revisitable at implementation. |
| 20 | §3 (n) | B. A trusty-tools directory that the workspace excludes, with its own lockfile and toolchain file. Revisitable at implementation. |
| 21 | §2.10 lifecycle set | B. Extraction provenance keys are not content. Sender and recipient fields stay content. Revisitable at implementation. |
| 22 | §5.2 | B. Stale-revision hits return flagged `stale_revision`. Hits from tombstoned or withdrawn concepts are dropped. Revisitable at implementation. |
| 23 | §3 (f); §2.2 | B. Block writes that break OKF §11 conformance. Report warnings. Revisitable at implementation. |
| 24 | §3 (h) | A. `publish = false` for now. Revisitable at implementation. |
| 25 | §3 (o); §2.16 "Tiers" | A. trusty-okf reads UTF-8 text and Markdown only. The worker handles every other format. Revisitable at implementation. |
| 26 | §3 (p); §2.16 | A. OCR and ML models off. A scanned PDF yields empty text and a `no_text_layer` note. Revisitable at implementation. |

Decisions that need another document amended are listed as pre-implementation filings in §6.1 items 9 and 13–16.

### 1.6 Owner decisions already in force

These are owner decisions of 2026-08-01 / 2026-08-03, recorded in DOC-63 §14:

- Each agent has exactly one canonical OKG.
- The OKG is internal, built (derived and rebuildable) and indexed by trusty-search.
- The OKG lives at `<trusty-agents home>/<agent>/okg`.
- Stores populate the OKG. A store is a pointer to a local directory or an extract from a connected system.
- trusty-memory's KG stays separate and unconnected.
- Trust labels and chunk-level fencing land before the connector grants ([#4546](https://github.com/bobmatnyc/trusty-tools/issues/4546)–[#4549](https://github.com/bobmatnyc/trusty-tools/issues/4549)).
- Scheduled refresh uses its own runner ([#4538](https://github.com/bobmatnyc/trusty-tools/issues/4538)).

The SENT-only rule of §1.3 is not in this list. The 06:58Z ruling narrows it to the automated OKG refresh (§1.5 item 2, §3 (l)).

## 2. Proposals: what trusty-okf owns and does not own {#SPEC-OKF-01-2}

### 2.1 Summary

trusty-okf owns the bundle and its content lifecycle. It parses, validates, writes, builds, migrates, corrects and imports bundles. It holds the generic code that lives in trusty-kb today (§2.7). It owns import normalization, provenance and the three revision states (§1.1–§1.3). It does not own authenticated source adapters, credentials, agent configuration, the agent home layout, search ranking or chunking. It does not link xberg; it calls the extraction worker (§1.4). It owns the feed loop, behind the cargo feature `search-feed` (§3 (e), decided A).

### 2.2 Parse and validate canonical OKF

- trusty-okf reads leniently. It accepts every bundle that OKF §11 calls conformant. It also opens non-conformant bundles and reports findings instead of refusing them, per OKF §12 "best-effort consumption" ([§11](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/SPEC.md#L736), [§12](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/SPEC.md#L766)).
- It never rejects a bundle for unknown types, unknown keys, broken links, missing optional fields or a missing `index.md`. OKF §11 forbids those rejections.
- It preserves unknown frontmatter keys on every round trip (OKF §4.1).
- It treats a bare `verified` mapping as a one-element list (OKF §11).
- Findings carry three severities: error, warning, info. This follows the wmahfoudh validator model ([CHECKS.md](https://github.com/wmahfoudh/okf/blob/7d6286ba112177394f08bf7b79e56f54a1fd22ec/skills-claude/okf-validate/CHECKS.md)). An error is an OKF §11 conformance breach. A warning is a §5–§10 field family with the wrong shape. An info is something §11 says consumers must accept.
- Writes are optionally strict. The default is `WriteMode::StrictErrors`: a write that breaks OKF §11 conformance is refused, and warnings and infos are reported (§3 (f), decided B; revisitable at implementation).
- No official validator, JSON schema or conformance corpus exists for OKF. The spec text is the only definition (external report §3).

#### 2.2.1 Raw-preserving round trip (r4) {#SPEC-OKF-01-2-2-1}

r4 fixes review BLOCKER 3 (C6). r3 parsed frontmatter into typed fields, so a value the types could not hold was lost on the next write.

**What must be true.** A read followed by any write keeps every frontmatter entry the write does not own byte for byte. This holds for malformed values, for OKF shapes the types do not model, and for unknown keys. A foreign file whose path fails the ID rule is never written.

**Raw is the source of truth.** The read model keeps the raw frontmatter text. It splits the text into top-level entries. An entry is the byte span from its key line to the line before the next top-level key, including comments and blank lines inside that span. Each entry keeps its parsed YAML value. The typed `Frontmatter` is a read-only view derived from the raw entries (§4.2). trusty-okf never renders a concept from the typed view alone.

**Typed views hold every value.** Each known key reads as one of three cases: absent, valid, or malformed. A malformed value keeps its raw YAML value and yields a warning finding. Examples:

- `stale_after: 2026-12-01` is date-only. It reads as malformed, and staleness reads as unknown (§2.4).
- `generated: {by: x}` has no `at`. OKF requires only `by` (`SPEC.md:377`), so it reads as valid with `at` absent.
- `status: archived` reads as `Status::Other("archived")` with a warning.
- `type: [a, b]` reads as malformed. A write under the default `StrictErrors` mode still refuses it (§3 (f)).

**Writes change only owned entries.** A write computes, per top-level key, whether the new parsed value differs from the stored one. It rewrites only the entries whose value changes. It removes only the entries the operation removes. It appends new entries after the last existing entry. Every other entry, and the body, keeps its bytes. An operation owns only the keys it sets: `deprecate` owns `status`; `verify` owns `verified` and the verification-record keys; a content change owns the caller's changed keys plus the §2.10 keys. A new concept has no stored entries, so it renders key-sorted (§4.3).

**Malformed owned values (r5).** An operation that owns `verified` or `generated` never drops or overwrites a malformed value of that key silently. Before it applies §2.10, it moves the stored value into an extension key: `verified` into `legacy_verified`, and `generated` into `legacy_generated`. The new entry keeps the old value's raw bytes unchanged under the new key name. The operation emits one `legacy_value_moved` warning finding per moved key. When the extension key already exists, the operation is refused with `Conflict` and the file stays unchanged. Both extension keys are in the §2.10 lifecycle set, so the move never changes the content hash.

**Unparseable frontmatter.** When the frontmatter is not valid YAML, or not a mapping, the concept reads with an error finding. Every write to it is refused with `Parse`, and its file stays unchanged (§4.4).

**Foreign paths.** A `.md` file whose bundle-relative path fails §2.11 is a foreign file. Examples are `notes/a:b.md`, `Index.md` and `_drafts/x.md`. The build directories `_sources/` and `_state/` hold no concepts. OKF reserves only `index.md` and `log.md` (`SPEC.md:137`), so such a file can be a conformant concept. `Bundle::open` reads it as `ConceptRef::Foreign` with an info finding. No operation writes, moves, deletes or rewrites links inside a foreign file. `move_concept` leaves a link inside a foreign file unchanged and reports it. A write that targets a foreign file is refused with `InvalidId`.

Criteria 113–115 and 125 test this section.

### 2.3 Create and update concepts and `index.md` files

- trusty-okf creates, updates and moves concepts.
- It refuses a concept named `index.md` or `log.md`. OKF §3.1 reserves those names. §2.11 gives the full ID rule.
- It regenerates `index.md` files from concept frontmatter. Each entry uses the linked concept's `description` (OKF §8). Only the bundle-root `index.md` carries frontmatter, and only `okf_version`.
- It overwrites an `index.md` only when the file's first line is a generator marker that trusty-okf or trusty-kb wrote. For the root `index.md`, which carries `okf_version` frontmatter, the marker is read from the first line after the frontmatter block (Amended 2026-11-05, #10746 D4b, Bob: "approve S3 amendments 1, 4, 5"). trusty-kb's marker is `structure.rs:22@c1eed98441`. A hand-written `index.md` stays unchanged, and trusty-okf reports an info finding (proposal). trusty-kb today rewrites any `index.md` whose content differs (`crates/trusty-kb/src/structure.rs:84-99@c1eed98441`).
- It appends to `log.md` for every lifecycle event. Date headings use ISO 8601 `YYYY-MM-DD`, newest first (OKF §9).
- A concept ID is its path, so a move changes the ID. trusty-okf rewrites every link to the old ID across the bundle in the same operation. It records the move in `log.md`.
- A write that changes nothing writes no bytes. trusty-kb's `write_entity_at` already compares before writing (`crates/trusty-kb/src/store.rs:129-141@c1eed98441`).

### 2.4 Source, verification and freshness metadata

trusty-okf writes the OKF v0.2 fields:

- `sources[]`: each entry has `resource` and, where the body cites it, `id` (OKF §5.1).
- `generated: {by, at}`: who produced the current content and when it last changed meaningfully (OKF §5.2).
- `verified[]`: a list of `{by, at}` events (OKF §5.2). §2.10 says when trusty-okf clears it.
- `stale_after`: an absolute ISO 8601 instant with an explicit offset (OKF §5.5).
- `status`: `draft`, `stable` or `deprecated`; absent means `stable` (OKF §5.4).

trusty-okf derives the OKF trust tier and never stores it (OKF §5.3). It treats a date-only or offset-less `stale_after` as a warning, and reports staleness as unknown for that concept. The Google reference agent returns "not stale" in that case instead (`src/reference_agent/bundle/document.py:130`, external report §3).

trusty-okf writes a `human:` actor into `verified` only when the caller passes an explicit human confirmation. A model-driven write never self-verifies as human. This adopts the wmahfoudh rule ([DESIGN.md:258](https://github.com/wmahfoudh/okf/blob/7d6286ba112177394f08bf7b79e56f54a1fd22ec/DESIGN.md#L258)).

**Extending `stale_after`. — Decided 2026-11-03 (owner): owner-choices item 19 B; revisitable at implementation.** A concept is human-reviewed when its current revision is Verified with a `human:` event (§4.9). On such a concept, a write that moves `stale_after` later is accepted only from a `human:` actor with explicit confirmation, or in the same operation as a `verify` of the current revision. r7.1: that `verify` carries the new value in `Verification.stale_after` (§4.2). The `verify` checks `seen` against the current revision, records the event and moves `stale_after` in one write. The value is not projected into the `verified[]` event. Any other writer's extension is refused with `Conflict { reason: "stale_after_extension" }`, and the file stays unchanged. Any writer may move `stale_after` earlier. Option A, any writer may extend it, was not chosen. An automated process does not extend a person's review. Criterion 146 tests this.

The trust label is separate from the OKF trust tier. The ingest engine derives the trust label from the store registration at write time. A connector cannot set its own label (`crates/trusty-kb/src/okg/ingest.rs:279-291@c1eed98441`). At retrieval, the label is read back from the concept file, and an unreadable label reads as `UntrustedExternal` (`crates/trusty-kb/src/okg/trust.rs:174-195@c1eed98441`). A hand edit of the `trust` key therefore changes the label that retrieval sees (inference from those two sites). §2.12 lists this under DOC-63. The verification record outside frontmatter is authoritative, and frontmatter holds a bounded copy of it (§3 (d), decided C; §2.10). The trust label stays a frontmatter key, as today.

§2.8 maps trusty-kb's current fields to v0.2.

### 2.5 The correction lifecycle (decided 3c A)

OKF v0.2 does not define supersession. It has only `status: deprecated` and a log convention (external report §2). The Google sample records a correction in prose only ([acme_retail/log.md:18](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/bundles/acme_retail/log.md#L18)). Today trusty-kb has no correction model beyond last-record-wins in the ledger and source tombstones (survey headline 5).

**Decided 2026-11-03 (owner): §3 (c) option A.** The model below is 3c A. §4.8 gives its state transitions and keeps the rejected options B and C for reference.

1. **Deprecate, never delete.** A published concept is never deleted. It gets `status: deprecated` and stays at its path for links and history. wmahfoudh uses the same rule ([DESIGN.md:273](https://github.com/wmahfoudh/okf/blob/7d6286ba112177394f08bf7b79e56f54a1fd22ec/DESIGN.md#L273)).
2. **Supersede.** The old concept gets `superseded_by: /<new-id>.md`. The new concept gets `supersedes: /<old-id>.md`. Both are bundle-absolute links (OKF §6.1). Both use extension keys that OKF §4.1 allows.
3. **Correct in place.** A fix that keeps the concept's meaning updates the body. It is a content change, so §2.10 applies: `generated.at` moves, and the current `verified[]` events leave the frontmatter for the verification record.
4. **Record.** Every deprecation, supersession and correction appends one `log.md` entry with a leading bold verb.

Source deletion is not part of this model. It is the existing tombstone convention, which §4.8 covers.

### 2.6 The search boundary

The feed pushes concept files to trusty-search and withdraws them. trusty-okf reads search results back through trusty-search's query method. trusty-okf never chunks, embeds or ranks. trusty-okf owns the feed loop, in the feature-gated module `trusty_okf::feed` (§3 (e), decided A). §5 gives the contract that binds it.

### 2.7 Consolidation: module mapping and the agent-glue boundary

**Decided 2026-11-03 (owner): §3 (a) option A.** Generic trusty-kb code moves into trusty-okf. Agent glue stays in trusty-agents. trusty-kb retires after the cut-over. [ADR-OKF-0001](../adr/0075-trusty-okf-consolidates-trusty-kb-and-owns-the-index-feed-loop.md) (filed as ADR-0075) records this as a proposed amendment to ADR-0041.

The existing OKF code is one crate, `crates/trusty-kb` (8,545 source lines, `publish = false`, v0.1.0, never released). A thin agent-side layer sits in `crates/trusty-agents`. Nothing OKF-specific lives in trusty-search, trusty-memory, trusty-mpm or trusty-mcp (survey headline 1).

#### 2.7.1 The boundary rule {#SPEC-OKF-01-2-7-1}

A reviewer checks each rule against the trusty-okf crate. A breach of any rule fails review.

| # | Rule | How a reviewer checks it |
|---|---|---|
| B1 | trusty-okf has no dependency on trusty-agents, trusty-mcp, trusty-mcp-services, trusty-gworkspace or any connector crate. This covers normal, dev and build dependencies. | `cargo tree -p trusty-okf -e normal,dev,build` lists none of them. |
| B2 | The dependency direction is one way: trusty-agents depends on trusty-okf. | Same `cargo tree` check, plus `cargo tree -i trusty-okf` lists trusty-agents as a dependent. |
| B3 | Every public entry point takes a bundle root (`&Path`) or a `Bundle` opened from one. No function derives a path from an agent name, slug, home directory or environment variable. | No `std::env::var` call in the library. No `knowledge_dir`, `TRUSTY_AGENTS_HOME`, `KB_KNOWLEDGE_DIR` or `home_dir` in `src/`. |
| B4 | trusty-okf knows nothing about the agent home layout. | No path literal `okg`, `stores/`, `.trusty-agents` or `trusty-agents` joined onto a path in `src/`. |
| B5 | trusty-okf never creates, deletes or names a search index. The caller passes the index ID. | No `search.index.create` or `search.index.delete` string and no `assistant-okg-` literal in `src/`. |
| B6 | trusty-okf holds no credentials and defines no connector. Connector-specific `Locator` variants (Gmail, Drive) are not in trusty-okf. | No OAuth, token or keychain dependency. `Locator` has no `Gmail` or `Drive` variant. |
| B7 | trusty-okf has no MCP surface of its own (ADR-0041 Decision 4, kept by the amendment). | No MCP tool descriptors and no `serve --stdio` binary in the crate. |
| B8 | The core is network-free. Any network code sits behind the cargo feature `search-feed`. | `cargo tree -p trusty-okf --no-default-features` has no async runtime and no socket client. |
| B9 (r3, proposal) | trusty-okf never links xberg. Extraction beyond the built-in tier goes through the worker protocol (§2.16). The worker client sits behind the cargo feature `extract-worker`. | `cargo tree -p trusty-okf -e normal,dev,build --all-features` lists no `xberg`, `ort` or `candle-core`. |
| B10 (r3, proposal; r6 scope) | trusty-okf adds no credential, token or account secret to the environment, arguments or input of any child process. The extraction worker starts with a cleared environment plus the §2.16 allow-list and no network setting. An acquisition adapter process (3m, decided A) starts with exactly the environment list its caller supplies, and trusty-okf adds no variable to it (§2.16). | No credential type in the worker client or the adapter spawn API. A test reads each child's environment: the worker holds only the allow-listed variables, and an adapter holds exactly the caller-supplied list (criterion 139). |

**r3 changes to the boundary (import scope).** Rules B1–B8 stay unchanged. Bob's import scope keeps the boundary (§1.3 item 22).

- **B6 is clarified, not changed.** trusty-okf defines the common import interface and the worker protocol. It implements one source adapter, the local `docstore` reader, which needs no credential. It implements no authenticated source adapter. Gmail and Google Docs adapters stay outside trusty-okf.
- **B9 and B10 are new.** They follow from the approved worker design (§1.4).
- **B10 scope (r6, reconciliation row 11).** What must be true: B10's cleared environment binds the extraction worker only. A cleared environment would leave a 3m A adapter without its network settings and its credential store, which it finds through variables such as `HOME`, so the public CLI could not import email or Google Docs (§1.3 item 23). The adapter spawn API therefore takes an explicit environment list from its caller, for example `HOME`, the credential-store path and proxy settings. trusty-okf passes that list through unchanged and adds nothing. The adapter holds its own grant (ADR-0026). r7: 3m is decided A, so the rule binds every adapter process. 3m C was ruled out by r6: it runs the adapter inside trusty-okf's own process, so no environment boundary exists, and it breaks B1 and B6. 3g is decided B, so the trusty-okf CLI is the caller that builds the list from its own arguments and configuration. The list stays a library parameter, so another host binary supplies its own.
- **The end-to-end CLI needs no rule change under 3m A.** The rejected 3m C would link connector code, which breaks B1 and B6. §3 (m) records this.

#### 2.7.2 Module mapping

Destinations name trusty-okf modules from §4.1. Each row cites the source at `@c1eed98441`.

| Source module | Destination | Note |
|---|---|---|
| `crates/trusty-kb/src/frontmatter.rs` | `trusty_okf::format::frontmatter` | Split, parse and key-sorted render. Gains v0.2 field types. |
| `crates/trusty-kb/src/entity.rs` | `trusty_okf::format::concept` | `Entity` becomes `Concept`. `slugify`, `deep_merge`, `wiki_links` and `link_values` move with it. |
| `crates/trusty-kb/src/schema.rs`, container rules (`RESERVED_FILES` at `:56`, type-only required, unknown-key preservation) | `trusty_okf::format::container` | Pure format rules. |
| `crates/trusty-kb/src/schema.rs`, `Profile`, `CollectionSpec`, `EDGE_PAIRS`, `inverse_edge` | `trusty_okf::profile` | Pluggable. The personal-KB profile (people, organizations, places, …, `:217-248`) ships as one profile, not as the container default (proposal). It names no agent, so it passes B3 and B4. |
| `crates/trusty-kb/src/store.rs` | `trusty_okf::bundle` | `KbStore` becomes `Bundle`. Its constructor takes the bundle root. |
| `crates/trusty-kb/src/put.rs` | `trusty_okf::bundle::put` | Writes v0.2 fields per §2.8 and applies §2.10. |
| `crates/trusty-kb/src/reconcile.rs` | `trusty_okf::profile::reconcile` | Inverse edges come from the profile. |
| `crates/trusty-kb/src/structure.rs` | `trusty_okf::bundle::index_md` | Becomes the OKF §8 `index.md` regenerator, with the marker rule in §2.3. |
| `crates/trusty-kb/src/validate.rs` | `trusty_okf::format::validate` | Gains the error/warning/info levels in §2.2. |
| `crates/trusty-kb/src/convert.rs` | **Dropped; does not move to trusty-okf** | trusty-okf has no `bundle::convert`. The lossless trusty-kb-to-v0.2 path is `bundle::migrate` (§2.9). Import takes its plan-then-apply shape from §2.13, not from this module. (Amended 2026-11-05, #10746 D14, Bob: "Amend §2.7.2 and §4.1 to drop convert.") |
| `crates/trusty-kb/src/roots.rs`, `confine`, `assert_within` | `trusty_okf::bundle::paths` | Generic. Strengthened per §2.11. |
| `crates/trusty-kb/src/roots.rs`, `Roots`, `resolve`, `tree_path` | **Stays in trusty-agents** | Maps an agent name to `<knowledge_dir>/<slug>` (`:25-65`). Agent home layout, so B3 and B4 exclude it. |
| `crates/trusty-kb/src/mcp.rs`, `tooldefs.rs`, `main.rs` | **Does not move to trusty-okf** | The `kb_*` MCP stdio server. B7 excludes it. ADR-0041 Decision 4 re-homes the agent-consumed tools in a trusty-mcp-services service, which is unbuilt. `kb_ensure_structure` and `kb_validate` become trusty-okf library calls. The binary is registered nowhere today (ADR-0040, ADR-0041 `:30`). |
| `crates/trusty-kb/tests/dispatch_tests.rs` | Follows `mcp.rs` | MCP dispatch tests. |
| `crates/trusty-kb/tests/okg_artifacts.rs` | trusty-okf tests | Build artifacts. |
| `crates/trusty-kb/src/okg/mod.rs` | `trusty_okf::build` | `okg_register_source`, `okg_sources`, `okg_index_coverage`, `okg_ingest_docstore`. Renamed without the `okg_` prefix (proposal). |
| `crates/trusty-kb/src/okg/registry.rs` | `trusty_okf::build::registry` | `Locator` keeps `DocStore`. `Gmail` and `Drive` (`:60-80`) leave trusty-okf through the open variant DOC-55 §5.3 proposes (`Connector { kind, params }`). trusty-agents owns the `gmail` and `drive` kinds. §2.9 requires that existing `registry.toml` rows still parse. |
| `crates/trusty-kb/src/okg/ledger.rs`, `jsonl.rs` | `trusty_okf::build::ledger`, `build::jsonl` | Append-only and torn-line tolerant. Unchanged on-disk format. |
| `crates/trusty-kb/src/okg/ingest.rs` | `trusty_okf::build::ingest` | Fetcher-agnostic engine. Changes per §2.9: it preserves editorial and verification keys across a re-ingest. |
| `crates/trusty-kb/src/okg/docstore.rs` | `trusty_okf::build::docstore` | Filesystem fetcher. Takes an explicit directory. |
| `crates/trusty-kb/src/okg/policy.rs` | `trusty_okf::build::policy` | Read-side allow-list. `home_default` takes the user home as an argument (`:70`), not the agent home. |
| `crates/trusty-kb/src/okg/trust.rs` | `trusty_okf::build::trust` | Label derivation and fail-closed read. |
| `crates/trusty-kb/src/okg/index_journal.rs` | `trusty_okf::build::index_journal` | Pure diff of tree against journal. Widened to cover every concept per §5.4. r4: the journal moves to the machine-local index state directory (§4.9.1). |
| `crates/trusty-common/src/knowledge_document.rs` | **Stays in trusty-common** | trusty-search depends on it (`crates/trusty-search/src/core/indexer/ingest/mod.rs:425@c1eed98441`). trusty-okf uses local constants instead, kept in step with it by a test. (Amended 2026-11-05, #10746 D4b, Bob: "Accept S4 local constants and amend §2.7.2.") |
| `crates/trusty-agents/src/stores/index_feed.rs` (`IndexFeed`, `covers`, `feed_source`) | `trusty_okf::feed` (3e, decided A) | Generic: it imports only trusty-kb types (`:62-64`). Behind the cargo feature `search-feed`. Not chosen: 3e B (stays in trusty-agents) and 3e C (trusty-search absorbs it). |
| `crates/trusty-agents/src/stores/index_feed_rpc.rs` (`RpcIndexFeed`) | `trusty_okf::feed` | Follows `index_feed.rs`. |
| `crates/trusty-agents/src/stores/index_feed_tests.rs` | trusty-okf tests | Follows `index_feed.rs`. |
| `crates/trusty-agents/src/stores/okg_graph.rs`, `read_graph` (`:162`) and the `OkgGraph` types | `trusty_okf::bundle::graph` | Takes a root path and reads frontmatter only. |
| `crates/trusty-agents/src/stores/okg_graph.rs`, `resolve_okg_root` (`:248`) | **Stays in trusty-agents** | Maps an agent binding to its tree. |
| `crates/trusty-agents/src/stores/binding.rs`, `config.rs`, `status.rs` | **Stays in trusty-agents** | `[[stores]]` configuration and tree-to-index binding. |
| `crates/trusty-agents/src/tools/okg/*` (including `gmail.rs`, `drive.rs`, `gapi.rs`) | **Stays in trusty-agents** | Agent tools, connectors and credentials. |
| `crates/trusty-agents/src/tools/memory/okg_fence.rs` | **Stays in trusty-agents** | Fence rendering reuses the agent's memory fencing (DOC-63 S-4.5). |
| `crates/trusty-agents/src/api/server/knowledge_pipeline/*` | **Stays in trusty-agents** | Extraction pipeline and protected `assistant-okg-*` index creation. |

**Call-site impact.** `grep -rn "trusty_kb::" crates` outside `crates/trusty-kb` finds 57 lines in 20 `.rs` files, all in trusty-agents, plus one changelog fragment. ADR-0041 counted 51 sites in 18 files on 2026-08-10 (`docs/adr/0041-…md:100@c1eed98441`). The Cargo edges are `crates/trusty-agents/Cargo.toml:111` and the workspace member at `Cargo.toml:75`. ADR-0041 cites `Cargo.toml:84`; the line is now `:111`.

### 2.8 OKF v0.2 compatibility: field mapping {#SPEC-OKF-01-2-8}

trusty-kb implements OKF v0.1 (`crates/trusty-kb/src/schema.rs:1@c1eed98441`). v0.2 renamed two fields: `timestamp` became `generated.at`, and the body `# Citations` list became `sources` ([SPEC.md §13](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/SPEC.md#L796)).

The table is the conversion rule. trusty-okf writes v0.2 and reads v0.1 through the OKF §13 fallbacks. It migrates an OKG by rebuilding it, and it migrates other concepts with `migrate` (§3 (b), decided B; §2.9). Every row obeys §2.9: a legacy key is removed only when its value is recoverable byte for byte from the new key.

| What trusty-kb writes today | Where it is written (`@c1eed98441`) | v0.2 form | Rule |
|---|---|---|---|
| `type` | `put.rs:69-75` (defaults to the collection type or `Note`) | `type` | Unchanged. |
| `title`, `description`, `tags` | `put.rs:76-79`, `schema.rs:118-131` | Same keys | Unchanged. |
| `aliases`, `same_as`, `uid` | `schema.rs:118-131` | Extension keys | Kept verbatim. v0.2 has no equivalent. `same_as` is not mapped to `resource`, because `resource` names the underlying asset, not an authority URI (inference from OKF §4.1). |
| `updated` | `put.rs:91-109` | `generated.at` | Mapped only when the value parses as an ISO 8601 datetime with an explicit offset. Then `updated` is removed. Otherwise `updated` stays, `generated` stays absent, and the validator reports a warning. `put_entity` takes `now: &str` and writes it verbatim (`put.rs:51`), so a date-only value is possible. trusty-okf never invents a time or an offset. |
| (none) | — | `generated.by` | v0.1 data has no producer. The migration writes `process:trusty-kb-legacy` when it maps `updated` (proposal). A later content change replaces it with the real writer. |
| `created` | `put.rs:85-90` | Extension key | Kept verbatim. v0.2 has no creation field. |
| `sources` as a list of plain strings | — | `sources[]` of mappings | Each string `s` becomes `{resource: s}`. Order is kept. No `id` is invented. An entry that is already a mapping passes through. |
| `source_id`, `source_kind`, `source_item_id`, `source_timestamp` | `okg/ingest.rs:276-294` | Extension keys, top level | Kept verbatim and at the top level. `is_tombstone` needs a top-level `source_id` (`crates/trusty-common/src/knowledge_document.rs:21-27@c1eed98441`). |
| Fetcher provenance fields (`url`, `source_path`, `from`, …) | `okg/ingest.rs:55`, `:296-302` | Extension keys, plus one `sources[]` entry | Kept verbatim. When a `url` or `source_path` field exists, ingest also writes one `sources[]` entry with that value as `resource` (proposal). No other source of a resource is invented. |
| `trust` | `okg/ingest.rs:283-291` | Extension key | Kept verbatim. Never raised by migration. Separate from the OKF trust tier. |
| `source_status: deleted`, `tombstoned` | `okg/ingest.rs:341-345` | Extension keys | Kept verbatim. §4.8 covers the lifecycle. |
| Relationship fields holding `[[wiki-links]]` | `schema.rs:24-26`, `entity.rs:189-229` | Extension keys | Kept verbatim. OKF links are markdown links (§6.1), so wiki-links are not OKF links. The validator reports them as info. |
| `timestamp` (foreign v0.1 bundles) | not written by trusty-kb | `generated.at` | OKF §13 read fallback. Same parse rule as `updated`. |
| Body `# Citations` (foreign v0.1 bundles) | not written by trusty-kb | `sources[]` | OKF §13 read fallback. The body is never edited; the derived `sources[]` entries are added (proposal). |
| Per-folder `index.md` as a name/summary table with `GENERATED_MARKER` | `structure.rs:22`, `:136`, `:163` | OKF §8 `index.md` | Regenerated, because it is a derived file. A hand-written `index.md` stays (§2.3). |
| Root `index.md` without `okf_version` | `structure.rs:163` | Root `index.md` with `okf_version: "0.2"` | Added on regeneration. |
| `_state/conversion-log.md` | — | Kept, plus `log.md` | Kept unchanged. No trusty-okf module writes it (Amended 2026-11-05, #10746 D14). trusty-kb writes no `log.md` today (grep at `@c1eed98441`). |

DOC-63 S-4.3 lists `ingested_at` as an existing frontmatter field. `write_item` does not write it (`okg/ingest.rs:276-294@c1eed98441`); the value lives in the ledger record (`okg/ledger.rs:59`). The mapping therefore has no `ingested_at` row.

### 2.9 Lossless migration and source preservation {#SPEC-OKF-01-2-9}

A migration takes an existing trusty-kb tree to the v0.2 form in §2.8. Under §3 (b), decided B, an OKG migrates by `rebuild`, which regenerates its ledger-owned concepts. `rebuild` never touches a hand-owned concept (§4.5), so the in-place `migrate` covers hand-owned concepts and bundles that are not build products. The rules below bind every migration path, including a rebuild.

**What a migration keeps.** Each item is checked by the audit below.

1. Every concept file, at its current path. A migration never moves, renames or deletes a file, so the concept ID set is unchanged. This includes hand-written concepts with no ledger record and tombstoned concepts.
2. Every concept body, byte for byte. A migration edits frontmatter only.
3. Every frontmatter key and value. A legacy key is removed only under the §2.8 rule. An unmapped key stays.
4. Every source reference: each `sources` string or mapping, each top-level `source_*` key, each fetcher provenance field, each ledger record, and each registry row.
5. Every verification event. A bare `verified` mapping becomes a one-element list with the same `by` and `at`.
6. The trust label. A migration never changes the `trust` key.
7. Every tombstone. `source_status` and `tombstoned` stay unchanged.
8. Log history: every `log.md`, `_state/conversion-log.md`, the ledger JSONL files and the index journals. A migration only appends to them. Git history in a per-store sync repo (ADR-0022) is untouched, because a migration never rewrites history.
9. Every hand-written `index.md` (§2.3).
10. Every `registry.toml` row, including rows with `gmail` or `drive` locator tables. trusty-okf reads them after `Locator` opens (§2.7.2). It keeps a row it cannot interpret, and reports it, instead of dropping it.

**How a migration detects loss.** A migration runs in two phases (proposal).

1. **Plan.** It reads the tree and writes nothing. It returns the exact list of per-file changes. (#10746 D14: no `convert_tree` precedent.)
2. **Apply.** A migration is an operation of kind `migrate` in the operation journal (§4.11). Before its first write, it stores a full backup of every file it will change (r4). Each backup is the complete old bytes, body included, stored content-addressed at `_sources/migrations/<run-id>/blobs/sha256-<hex>`. The manifest `_sources/migrations/<run-id>.jsonl` holds one line per changed file: the path, the SHA-256 of the old bytes, the backup blob name and the SHA-256 of the new bytes. Backups and manifest are synced to disk before the intent record. Then the migration writes each file atomically (§4.4).
3. **Audit.** After the apply phase, it compares the old inventory with the new one. It checks: equal concept ID sets; equal body bytes; every old key present with an equal value, or mapped by a §2.8 row whose inverse yields the old value; every old `sources` value present as a `resource`; equal verification event multisets; ledger, registry and journal files equal or only appended; and each old log file present as one contiguous substring of the new file.
4. **Failure.** Any audit failure restores every changed file from its backup blob. Restore writes the blob atomically, then checks that the restored file's SHA-256 equals the manifest's old hash. The migration then returns `OkfError::MigrationLoss` with every failing item. A restore whose check fails adds a `restore_failed` item, and the blob stays in place. The migration reports no partial success.
5. **Interruption (r4).** A migration interrupted before its completion record rolls back on the next `Bundle::open`. Recovery restores every changed file from its backup and checks each old hash, as in step 4 (§4.11). The audit never ran, so a roll forward would skip the loss check.
6. **Cleanup (r4).** After a passed audit, the completion record is written. Then the backup blobs are removed. The manifest stays as history (item 8).

**What must be true (r4, review BLOCKER 6).** Until a migration completes, its manifest and backups restore every changed file to its exact old bytes. Criterion 120 tests restore on a changed body.

**No silent drop.** A value that trusty-okf cannot map stays under its legacy key, and the validator reports a finding. trusty-okf never discards a value to make a concept conformant.

**Idempotency.** A second migration over a migrated tree plans zero changes and writes zero bytes.

**Two existing behaviours this section changes.**

- **Re-ingest overwrites frontmatter.** `write_item` builds a fresh mapping and calls `put_entity` with `merge = false` (`crates/trusty-kb/src/okg/ingest.rs:262-312@c1eed98441`). Only `created` survives (`put.rs:85-90`). A re-ingest of a changed item therefore drops `verified`, `status`, `supersedes`, `superseded_by` and any hand-added key. trusty-okf's ingest keeps a fixed set of editorial keys across a re-ingest: `status`, `supersedes`, `superseded_by`, `stale_after` and the verification record (proposal). It still replaces the source-derived keys, which is the stale-provenance guard `write_item` documents. Verification events still follow §2.10.
- **Rebuild deletes concepts.** r1 §4.5 said `rebuild` deletes and regenerates every `.md` file. That deletes hand-written concepts and hand edits, and DOC-63 S-1.2a expects external edits (#4325). §4.5 now limits a rebuild to ledger-owned concepts. The decided 3b B says "migrate an OKG by rebuilding it". It is lossless only under the revised §4.5.

**Path collisions (r6, reconciliation row 8).** The defect is confirmed. `chunk_items` names an item from its relative path minus the extension (`crates/trusty-kb/src/okg/docstore.rs:172@c1eed98441`). `entity_path` slugifies that name (`crates/trusty-kb/src/store.rs:95-99@c1eed98441`), and `slugify` folds every non-alphanumeric run to `-` (`crates/trusty-kb/src/entity.rs:92-111@c1eed98441`). So `a.md` and `a.txt` map to one file, and so do `nested/c.md` and `nested-c.md`. Today the second item overwrites the first, and `BuildReport` counts both as written.

What must be true: `build`, `rebuild` and `import` never write two items to one concept path.

- Before any write, the engine maps every item of the run to its concept path. It also checks each path against the paths the ledger already pins to other items.
- Two items of one run that map to one path are both refused. Each is reported as failed with reason `collision`, and neither is written.
- An item that maps to a path the ledger pins to a different item is refused with `collision`. The pinned concept keeps its bytes.
- The check uses the fold rule of §2.11, so two paths that differ only in case or Unicode normalization also collide.
- `BuildReport` counts a refused item as failed, never as written.

The migration audit still reports any concept path that two ledger item IDs claim. Criterion 138 tests this rule.

### 2.10 Verification invalidation on content change (proposal) {#SPEC-OKF-01-2-10}

A verification event attests one revision of a concept. When the content changes, the attestation no longer covers it.

**Decided 2026-11-03 (owner): owner-choices item 8 A. This is a trusty policy, not an OKF requirement.** OKF §5.2 says `verified` is independent of `generated.at`, so OKF lets a writer keep `verified` entries across a content change. OKF §5.3 derives the tier from `verified` alone. If trusty-okf kept the entries, other OKF readers would show an old review on new content. So trusty-okf removes them from frontmatter on every content change, and keeps the old review as history in the verification record. Option B, keep the entries and change only trusty's own verification state, was not chosen.

**Content hash.** The content hash is SHA-256 over a canonical rendering of two parts:

1. The body, with CRLF line endings normalized to LF.
2. The content frontmatter: every key except the lifecycle set, rendered key-sorted, after the §2.8 normalization. Plain-string `sources` normalize to `{resource: …}` mappings first, so a migration never changes the hash.

The lifecycle set holds keys that describe the concept's state, not its claims: `generated`, `verified`, `stale_after`, `status`, `supersedes`, `superseded_by`, `trust`, `source_id`, `source_kind`, `source_item_id`, `source_timestamp`, `source_status`, `tombstoned`, `created`, `updated`, `legacy_verified`, `legacy_generated` (§2.2.1, r5), the extraction provenance keys (r7, below), and the verification-record keys. Every other key is content, including unknown keys and `sources[]`. trusty-kb's `content_signature` already excludes `created` and `updated` the same way (`crates/trusty-kb/src/put.rs:194-203@c1eed98441`).

**Extraction provenance. — Decided 2026-11-03 (owner): owner-choices item 21 B; revisitable at implementation.** The extraction provenance keys of §2.14 rule 5 and §2.16 are in the lifecycle set: `extracted_by`, `extract_protocol`, `xberg_version`, `extract_config` and the input SHA-256 key. So a worker upgrade that yields the same text keeps the content hash and the review. A worker upgrade that yields different text changes the body, and §2.10 applies. Source fields such as the sender, the recipients and the subject stay content. Option A, extraction keys count as content, was not chosen. Criterion 147 tests this.

**What a content change does.** When a write changes the content hash, trusty-okf does five things in the same write.

1. It sets `generated.at` to the caller's `now` and `generated.by` to the writer's actor.
2. It moves every current `verified[]` event into the verification record. Each moved event gains `invalidated_at` (the write's `now`) and `invalidated_by_hash` (the new content hash).
3. It removes `verified` from the frontmatter. The derived OKF trust tier becomes unverified.
4. It appends one `log.md` entry that names the concept and the number of cleared events.
5. It leaves the trust label unchanged. The label belongs to the store, not to the revision.

A write that changes only lifecycle keys does none of this. `deprecate`, a tombstone and a `stale_after` renewal keep `verified[]`.

**Binding a verification to a revision.** `verify` takes the content hash that the verifier saw. trusty-okf refuses the call with `Conflict` when the current hash differs. A human therefore cannot verify a revision that changed after they read it. The verification record stores the hash with each event.

**Out-of-band edits.** A hand edit, a git sync merge (ADR-0022) or another tool can change content without trusty-okf. On the next read, trusty-okf compares the current hash with the hash stored for the frontmatter events. On a mismatch, it reports the verification state as Unverified, with a warning finding. The OKF trust tier stays derived from the raw frontmatter (§4.9). The next trusty-okf write applies the five steps above. Until then, a non-trusty consumer that reads the raw frontmatter still sees the old events. That is residual risk.

**Where the record lives. — Decided 2026-11-03 (owner): §3 (d) option C.** The verification record is a ledger-side file under `_sources/`, and it is authoritative. Frontmatter holds a bounded copy: current `verified[]` events plus a bounded history projection. Only events in the record count toward Verified (§4.9 Axis 3). An event that appears only in frontmatter never counts as Verified, though the OKF trust tier still reads it (§4.9). Ruling D64 (#10873): a frontmatter-only event with no `hash` key does count in `history`, as "Events with no hash" states. A `rebuild` rewrites frontmatter and re-projects it from the record, so a human verification survives the rebuild. Not chosen: 3d B, the record only with no `verified` in frontmatter. Ruled out by r6: 3d A as written, the whole record in frontmatter.

**Bounded frontmatter (r6, reconciliation row 14).** The verification events in frontmatter are a bounded projection. The projection holds the current events and at most the 10 newest history events, and at most 16 KiB of rendered entries (proposal). Every event beyond the bound lives only in the verification record, and Axis 3 reads that record (§4.9). Under the decided 3d C the record holds the overflow. The rejected 3d A had no place for it. §4.8 states the 64 KiB tombstone bound that this rule serves.

**Events with no hash (r6, reconciliation row 17).** Every verification event written before r3 carries no content hash. What must be true:

1. An event with no hash is history. It never counts as Verified. Ruling D64 (#10873): this holds wherever the event lives. A frontmatter `verified[]` entry with no `hash` key whose `(by, at)` pair no folded record line shares is counted in `history`, once per distinct pair, even when the record does not hold it. It cannot make the state Verified and cannot raise a `VerificationMismatch` finding. An entry with a `hash` key, or without a string `by` or `at`, adds nothing. The code is `counted_events` in `crates/trusty-okf/src/bundle/record.rs`.
2. No operation assigns a hash to such an event. That covers `Bundle::open`, `migrate`, `rebuild`, recovery and `adopt`. §3 (q) is decided C: the equality proof runs first, then `adopt`. Neither may bind a legacy event to the current hash. Each proves byte ownership only.
3. The next content change moves every hashless event from frontmatter into the verification record, as step 2 above does, with the reason `legacy_unbound`.
4. A lifecycle-only write, a migration and a tombstone leave hashless events where they are.

So the OKF trust tier can still read `HumanReviewed` from such an event, while the verification state reads Unverified (§4.9). Criteria 142 and 143 test this.

**Moves.** `move_concept` rewrites links in other concepts' bodies. By the hash rule, each rewrite is a content change that clears that concept's verification. This is conservative. An exemption needs a link-normalized hash, which this spec does not design.

### 2.11 Path confinement {#SPEC-OKF-01-2-11}

Every trusty-okf write stays under the bundle root. The rules apply to concept files, `index.md`, `log.md`, `_sources/` files, temporary files and the lock file.

**Concept ID rule (lexical).** `ConceptId::parse` refuses an ID with `OkfError::InvalidId` when any of these holds:

1. It is empty, or it is not valid UTF-8.
2. It starts with `/` (absolute), or it contains `\` anywhere.
3. Any segment is empty, `.` or `..`.
4. Any segment contains `:`. This rejects Windows drive forms such as `C:` and `C:foo` on every platform.
5. It contains NUL or a control character.
6. Its last segment equals `index` or `log`, compared case-insensitively. macOS file systems are case-insensitive by default, so `Index.md` would collide with `index.md` (inference).
7. Its first segment starts with `_` or `.`. `_`-prefixed directories hold build machinery (#10746 D14: `convert.rs` cite removed), and `.`-prefixed segments hold `.git` and other tool state.
8. It ends in `.md`. The ID never includes the suffix.

This is stricter than today's `confine`. On Unix, `Path::components` returns `C:\x` as one `Normal` component, so `confine` accepts it (`crates/trusty-kb/src/roots.rs:75-89@c1eed98441`). Rule 4 closes that gap.

**Fold rule (r6, reconciliation row 13).** Default macOS file systems ignore case and Unicode normalization. So `Notes/A` and `notes/a`, or an NFC and an NFD spelling of one name, address one file under two IDs. Rule 6 folds only `index` and `log`, and §2.14 rule 4 covers import only.

What must be true: no write creates a second ID for a file that an existing ID already addresses.

- The fold key of an ID is its NFC form after Unicode full case folding.
- An existing ID is a concept file on disk, as the directory listing names it, or an ID that a ledger record pins.
- `put`, `move_concept`, `supersede`, `build`, `rebuild` and `import` refuse a target ID whose fold key equals the fold key of a different existing ID. The refusal is `Conflict { reason: "fold_collision" }`, and no file changes.
- The rule is lexical, so it gives the same answer on case-sensitive and case-insensitive file systems.
- `Bundle::open` reports two existing files with one fold key as a warning finding, and reads both.

Criterion 140 tests this rule.

**File system rule.**

1. `Bundle::open` canonicalizes the bundle root once. The root must exist. Today `assert_within` skips the check when the root does not exist (`roots.rs:103-106`); trusty-okf refuses instead.
2. trusty-okf checks every component from the root to the target with `symlink_metadata`. It refuses with `PathEscape` when any existing component is a symbolic link, even one that resolves inside the root. Today `assert_within` allows an in-root link (`roots.rs:107-126`).
3. A write goes to a temporary file created with `create_new` in the target's own directory, then renames it over the target. A rename replaces a link instead of following it (inference from POSIX `rename`).
4. Between the check and the rename, another process can swap a directory for a link. A descriptor-relative open (`openat` with `O_NOFOLLOW`) closes that window. Rust's standard library does not expose it; a crate such as `cap-std` does (inference). Until then this window is residual risk.
5. A bundle walk never follows links. trusty-kb's docstore walk already pins `follow_links(false)` (`okg/docstore.rs:85-91`). A symbolic link found in a bundle produces a finding and is not read.

**Link rule.**

1. trusty-okf writes only bundle-absolute links built from a parsed `ConceptId` (`/<id>.md`). Such a link cannot leave the bundle.
2. `move_concept` rewrites a link only when the link resolves, lexically, to a concept inside the bundle. A relative link whose `..` segments climb above the root stays unchanged, and trusty-okf reports it.
3. trusty-okf never rewrites a URL.
4. `supersedes` and `superseded_by` values must parse as concept IDs. Otherwise the write is refused.
5. trusty-okf never dereferences a path-valued field (`resource`, `sources[].resource`, `computation`) for writing.

**Sidecar and feed paths.** Ledger paths stay `_sources/<slug(id)>.jsonl`, built with `confine` (`okg/ledger.rs:106-111`). r4: the index journal leaves the bundle (§4.9.1). trusty-okf writes no new `_sources/<slug(id)>.index.jsonl`. The feed sends only index-root-relative paths whose components are all `Normal` (`crates/trusty-agents/src/stores/index_feed_rpc.rs:13-28@c1eed98441`). It never sends a path under `_sources/` or `_state/` (§5.3, §5.3.1).

### 2.12 Existing ADR and spec boundaries {#SPEC-OKF-01-2-12}

| Document | What it still governs | Effect on trusty-okf |
|---|---|---|
| ADR-0040 | The consumer criterion: an agent-consumed tool goes to MCP, a framework-consumed tool stays native. The `trusty-mcp` / `trusty-mcp-services` split. | trusty-okf is framework-consumed, so it is native. Its CLI (3g, decided B) is a native entry point. |
| ADR-0041, as amended by ADR-OKF-0001 | Kept: the crate stays native (Decision 1, minus the name); trusty-search owns OKF indexing (Decision 2); each OKG store is a distinct named search index (Decision 3); a trusty-mcp-services service fronts agent reads and depends on the native crate (Decision 4); `kb_put_entity` stays unresolved. Replaced: the `trusty-okg` name and the single-crate rename path. | B7 follows from Decision 4. 3e is decided A, which contradicts Decision 2 for the feed loop. A further ADR-0041 amendment must move feed-loop ownership to trusty-okf, while trusty-search keeps chunking, embedding and ranking (§6.1 item 9). The rejected 3g C would have conflicted with Decision 4; the decided 3g B does not. |
| ADR-0022 | Tree content syncs through optional per-store git repos (`sync_remote`), with auto-commit on ingest. | trusty-okf does not sync. A sync merge is an out-of-band edit (§2.10). The lock file and temporary files must stay out of sync commits. Which ignore list carries that is ADR-0022's sync machinery (inference). r4: the operation journal directory `_sources/ops/` stays out of sync commits too, and a sync commit must not run while the bundle lock is held (§4.11). The index journal lives outside the synced tree (§4.9.1). The `trusty-kb-<tree>` repo name is stale prose (ADR-0041 `:116`). |
| DOC-55 (Draft) | The `Connector` contract and the open `Locator` variant (§5.3). The `Extractor` contract, E1–E5 and the extractor-version fingerprint (§4.2, §4.5). SPEC-OKGIMPORT-06: the ledger never waits on the index (§7.2.4). | §2.7.2 uses the open variant to move Gmail and Drive out. §5 keeps the ledger independent of the feed. Import adopts `Connector` and `Extractor` as its two adapter contracts (§2.13). The worker needs a DOC-55 amendment: DOC-55 §8 item 3 forbids "shelling out to an external converter" (`docs/specs/okg-universal-importer.md:603-606@c1eed98441`), and §3.3 excludes OCR (`:132-135`). §2.16 lists the exact changes. r7: 3p is decided A, so §3.3 stays unchanged. The §8 item 3 amendment is still needed and is a pre-implementation filing (§6.1 item 6). |
| ADR-0024 (Accepted, amended by 0058) | A sub-agent is an in-process leaf. It "never talks out-of-process" (`docs/adr/0024-…md:61-63@c1eed98441`). DOC-63 reads it as "must not be given an out-of-process ingestion path" (`DOC-63-okg-sources.md:1714`). | Import spawns the extraction worker and, under the decided 3m A, an acquisition adapter process. **Decided 2026-11-03 (owner): owner-choices item 3 A.** A sub-agent gets the in-process tier only: text and Markdown from a local tree, with no worker and no adapter process (§2.15 "Sub-agent imports"). ADR-0024 stays unchanged, so no amendment is filed. Not chosen: item 3 B (amend ADR-0024 for the worker and adapters) and item 3 C (no import at all for a sub-agent). |
| ADR-0026 / DOC-45 §6 (owner 2026-08-01) | A credential grant does not survive delegation. A sub-agent holds its own grant. | trusty-okf holds no grant (B6, B10). Whoever runs an authenticated source adapter holds its own grant under its own principal (§2.15). |
| ADR-0029 | Workspace MSRV 1.94. | xberg needs Rust 1.98 (Xberg assessment `:22`). The worker cannot build inside the 1.94 workspace build. §3 (n) is decided B: the worker builds in a trusty-tools directory that the workspace excludes, with its own toolchain file, and the workspace MSRV stays 1.94. Before implementation, a check confirms that the exclusion keeps the 1.94 workspace build clean, and ADR-0029 records the excluded directory as outside its policy (§6.1 item 15). |
| Xberg assessment (ar-T05, 2026-11-03) | A fit assessment, not a decision. It recommended "no addition" and an in-process plugin framework first (`:5`, `:314`). | The 03:17Z owner choice supersedes that recommendation for import. The 06:58Z ruling settles the assessment's open points: worker location (3n B), built-in tier (3o A), OCR off (3p A) and an OS sandbox before email or Drive content (item 9 B). §2.16 records the reconciliation item by item. |
| DOC-63 (Accepted) | One canonical OKG per agent at `<home>/<agent>/okg` (§2.2). Trust labels: engine-written (S-4.3), carried to chunks (S-4.4, unbuilt), fenced (S-4.5), fail-closed (S-4.6). Idempotency and watermarks (§8). Scheduled refresh (§9). Push feed as the correctness path and the daemon watcher as a backstop (S-8.1). One index per store (S-8.2). System-created store index (S-8.4). | trusty-agents owns the layout, the binding and index creation (B3–B5). trusty-okf owns S-4.3 at write and S-4.6 at read. S-4.4 stays a trusty-search gap (§5.5). The watcher can re-index a withdrawn concept, so the decided 3i A re-checks withdrawn paths on every feed pass (§5.4 F9). r7: S-3.3 (SENT-only) is amended to scope it to the automated OKG refresh (§3 (l), §6.1 item 13; owner item 2 C; amended in [#10810](https://github.com/bobmatnyc/trusty-tools/pull/10810) under [#10742](https://github.com/bobmatnyc/trusty-tools/issues/10742)). |

### 2.13 Import: one engine, two adapter contracts {#SPEC-OKF-01-2-13}

Scope is approved (§1.2, §1.3). The design below is a proposal.

**One engine.** An import registers a store in the target bundle's registry and runs the existing ingest engine over it. It is not a second engine. The engine already gives idempotency, additive registration, crash convergence, fail-closed reads and reported deletions (DOC-55 §2.2, `docs/specs/okg-universal-importer.md:76-87@c1eed98441`). An OKG build and an import are the same operation with different callers. An OKG build names the agent's OKG as its bundle; an import names any bundle root (B3).

**Two adapter contracts, both from DOC-55.**

1. A **source adapter** is DOC-55's `Connector`: `list` returns descriptors, `fetch` returns blobs, and the engine owns dedup, tombstones and writes (DOC-55 §5.1–§5.2, `:292-359`). Obligations C1–C8 apply unchanged.
2. An **extractor** is DOC-55's `Extractor`: bytes and a type hint in, `Extracted { text, fields, notes }` out (DOC-55 §4.2, `:179-222`). Obligations E1–E5 apply, with the worker amendments in §2.16.

trusty-okf adopts both contracts. It defines no third contract. The Xberg assessment's `DocumentExtractor` trait (`xberg-assessment.md:105-137`) would be a duplicate, so this spec does not adopt it (§2.16).

**r3 changes to DOC-55's `Connector` (proposal).** `list` returns a resumable cursor and an explicit `complete` flag. `fetch` returns each item's immutable source ID, source revision and source dates. Both changes serve resumability and complete pagination (§2.15). Whether the trait is synchronous or asynchronous is an implementation choice. Either way the core takes no async runtime dependency (B8).

**Listing completeness (r6, reconciliation row 3).** The engine tombstones on a complete listing (§2.15 item 8). Today `gmail_list_ids` returns at the ceiling with no truncation flag (`crates/trusty-agents/src/tools/okg/gapi.rs:134-137@c1eed98441`), and Drive completeness also depends on the ceiling (`crates/trusty-agents/src/tools/okg/drive.rs:216-219@c1eed98441`). An adapter that stops at exactly the ceiling and reports `complete: true` would make the engine tombstone every live item past the ceiling.

What must be true: no tombstone rests on a listing that the engine cannot show is whole.

- The run report carries two fields, `pagination_exhausted` and `deletion_safe`. They replace r5.1's single `complete_listing`.
- The engine computes `pagination_exhausted` itself. It is false when the adapter reported a remaining page token or cursor, when the listed count reached the run's item ceiling, or when the adapter reported `complete: false`. The adapter's `complete` flag alone never makes it true.
- `deletion_safe` is true only when `pagination_exhausted` is true, the store kind supports deletion detection, and the store sets `tombstone_deleted` (C4). Drive also needs a recursive listing.
- A Gmail store is never deletion-safe, because its listing is a window (DOC-55 C4).
- The engine tombstones an item only when `deletion_safe` is true. Otherwise an absent item is reported as `missing`.

So an operator can tell a Gmail run that read every page (`pagination_exhausted: true`, `deletion_safe: false`) from one cut at the ceiling (`pagination_exhausted: false`). Criterion 132 tests this.

**Full-ID addressing (r6, reconciliation row 6).** Today `tombstone_item` rebuilds the target path from the last `/` segment of the ledger entity plus the store collection (`crates/trusty-kb/src/okg/ingest.rs:328-335@c1eed98441`). Import assigns multi-segment IDs such as `notes/sub/a` and `mail/<source-id>/<thread-id>/<message-id>`. Reused unchanged, the engine would tombstone `<collection>/a`, which can be a different concept.

What must be true: the engine addresses every concept by the full `ConceptId` that its ledger record pins. This holds for write, tombstone, tombstone reversal and lookup. The engine never rebuilds a path from a slug, a last segment or a collection name. Criterion 136 tests this.

**Reuse survey.** Each row cites trusty-tools at `@c1eed98441`.

| Existing code | What import reuses | What import does not reuse, and why |
|---|---|---|
| `crates/trusty-kb/src/convert.rs` | The plan-then-apply shape with a write-nothing default (`report_only`, `:89-121`). Fail-open handling of malformed frontmatter (`:167-181`). Type assignment: existing `type`, then collection default, then `Note` (`:199-205`). Title from the file stem (`:207-210`, `humanize` `:373-385`). Unknown-key preservation (`:286-288`). | Its in-place apply. `convert_tree` rewrites the tree it reads and deletes moved sources (`std::fs::remove_file`, `:249`), so it cannot keep a source tree unchanged. Its collection relocation (`:213-216`), because import preserves source hierarchy (proposal). Its `.md`-only filter (`:141`). |
| `crates/trusty-kb/src/okg/docstore.rs` | The tree reader: sorted walk with `follow_links(false)` (`:85-91`), extension filter, binary skip, the 8 MiB file cap (`MAX_FILE_BYTES`, `:40`), SHA-256 fingerprints. | Name derivation from relative paths (`:180-197`). It feeds `slugify`, which can collide (§2.9). Import derives IDs by the rule in §2.14. r6: the 20,000-character part splitting (`:36`, `:179-186`). Import reads each file whole (§2.14 rule 3). |
| `crates/trusty-kb/src/okg/policy.rs` | `DocStorePolicy`: operator-set read roots and hidden-segment rejection, re-checked at access. | — |
| `crates/trusty-kb/src/okg/ingest.rs`, `ledger.rs`, `registry.rs` | `SourceItem`, the ledger skip test `is_current`, tombstones, `upsert`, and the editorial-key preservation of §2.9. | — |
| DOC-55 §4.5 | The fingerprint tag `sha256:<hex>/<extractor-id>:<extractor-version>` (`:260-261`). | — |
| DOC-55 §6.3 | Progress derived from the per-store ledgers, not a second durable state (`:470-475`). | — |
| `crates/trusty-agents/src/tools/okg/gmail.rs`, `drive.rs`, `gapi.rs` | The Gmail and Drive adapters. They stay in trusty-agents (§2.15). | — |

#### 2.13.1 Protected keys at the ingest boundary (r4) {#SPEC-OKF-01-2-13-1}

r4 fixes review BLOCKER 4. The fix applies the approved requirement §1.1 item 12: only evidence that matches the current revision makes it verified.

**The gap today.** `write_item` copies every fetcher field that the envelope did not already set (`crates/trusty-kb/src/okg/ingest.rs:296-302@c1eed98441`). The envelope sets only `title`, the `source_*` keys it has values for, and `trust`. A fetcher field named `verified`, `status`, `superseded_by`, `source_status` or `generated` therefore lands in frontmatter. Import also reuses convert's unknown-key preservation (`crates/trusty-kb/src/convert.rs:286@c1eed98441`), which copies a source file's own frontmatter keys. Under the rejected 3d A an adapter could plant a verification-record hash equal to the deterministic content hash, and the concept would read as Verified. Under the decided 3d C a planted key cannot set the record, but it could still show a false review to OKF readers through frontmatter. The rule below covers both.

**What must be true.** No value from a source file, a source adapter or the extraction worker sets a protected key. Current-revision Verified comes only from events that trusty-okf's `verify` recorded.

**The protected set.** It holds every key in the §2.10 lifecycle set, every verification-record key, every key with the prefix `verified_`, and every key trusty-okf uses to record save, index or verification state. The set is one constant in `trusty_okf::format`, and the ingest boundary reads it. The §2.10 lifecycle set is a subset of it, so a change to the lifecycle set changes the protected set too.

**The rule.** The ingest engine checks every field from a fetcher, an extractor's `fields`, and a source file's preserved frontmatter. It does this before it merges them, and it applies unknown-key preservation only after this check. A field whose key is in the protected set is quarantined. Quarantine moves the key and its value, unchanged, into the extension mapping `source_fields`. The import report lists one `quarantined_key` finding per key and item. The engine then writes the protected keys itself: the envelope keys, `trust` from the store registration, and `generated` with the importer's actor. It never writes `verified`, `status`, `supersedes`, `superseded_by`, `stale_after` or a verification-record key from imported data.

**Why quarantine and not drop.** §2.9 forbids a silent drop. A nested key under `source_fields` keeps the source's value, and no OKF consumer or trusty reader treats it as a lifecycle key.

**Scope.** The rule binds every caller of the ingest engine: `build`, `rebuild`, `import` and every source adapter, including a 3m A adapter process. Editorial keys that a re-ingest keeps (§2.9) come from the existing concept, never from the item, so the rule does not touch them.

Criteria 84, 116 and 117 test this section.

### 2.14 Document-tree import (proposal) {#SPEC-OKF-01-2-14}

Scope is approved (§1.2). Every rule below is a proposal for review.

A document-tree import reads one local root and writes one bundle. The local root is a `docstore` store.

1. **The source tree stays unchanged.** Import opens the source read-only. It never creates, writes, moves, renames or deletes anything under the source root. It refuses with `OkfError::SourceOverlap` when the bundle root and the source root contain each other. Otherwise an import would read its own output (inference).
2. **Hierarchy is preserved.** Each source directory becomes the same ID path segment. Import does no collection relocation.
3. **IDs are deterministic.** The rule maps a source-relative path to a concept ID:
   - A `.md` source keeps its relative path without `.md`. `notes/a.md` becomes `notes/a`.
   - Any other source keeps its full file name. `notes/a.pdf` becomes `notes/a.pdf`, written as `notes/a.pdf.md`. So `a.md` and `a.pdf` never collide.
   - A segment that §2.11 refuses is normalized: NFC, refused characters and a leading `_` replaced by `-`, and a suffix `~<first 8 hex of SHA-256 of the full source-relative path>`. A source `index.md` becomes `index~<h8>`.
   - A path that is already a valid ID maps to itself. So the rule is the identity for ordinary trees.
   - **One source file yields one concept (r6, reconciliation row 7).** The docstore fetcher splits a file over 20,000 characters into items `<rel>#<n>` (`crates/trusty-kb/src/okg/docstore.rs:179-186@c1eed98441`). Under the one-path-one-ID rule, those parts would collide, and every large file would be refused. What must be true: import calls the docstore reader with part splitting disabled. Each source file becomes exactly one ledger item, with the source-relative path as its item ID, and one concept that holds the whole extracted text. The 8 MiB input cap (§2.16) still bounds the file. A vanished source file tombstones the one concept it produced. Criterion 137 tests this.
4. **Collisions are refused, not guessed.** Two source paths can still map to one ID, for example names that differ only in case on a case-insensitive file system (§2.11 fold rule). The plan lists every collision. Import writes neither colliding item and reports both as failed with reason `collision`. A ledger record pins an assigned ID, so a later source file cannot take over an existing concept's ID. r6: the same rule binds `build` and `rebuild` (§2.9 "Path collisions").
5. **Provenance is recorded.** Each concept gets `sources[]` with `{resource: <source-relative path>}`, and the top-level keys `source_id`, `source_kind: docstore`, `source_item_id` (the source-relative path) and `source_timestamp` (the file's modification time). A non-text source also gets `extracted_by: <extractor-id>@<version>`, which DOC-55 Q3 recommends (`okg-universal-importer.md:656`). Import writes no absolute path into the bundle. The registry row holds the root.
6. **Preview writes nothing.** `plan_import` returns the full plan: each item's ID, action, extractor and every refusal. It writes no byte. (#10746 D14: no `convert_tree` precedent.)
7. **Repeat import is idempotent.** An unchanged item (same fingerprint) is skipped. A changed item updates its concept, keeps the editorial keys (§2.9) and moves verification to history (§2.10). A concept that was hand-edited since its last import is refused with `Conflict` and reported, as `rebuild` does (§4.5). The hand-edit test is the §4.5 ownership proof, so a trusty-okf write is never a hand edit (r6). A vanished item is tombstoned only when the run's `deletion_safe` is true (§2.13 "Listing completeness", r6). A renamed source file is a tombstone plus a new concept. The report flags a pair with equal fingerprints as a possible move.
8. **Unsupported formats are reported.** Each skipped item carries one reason: `excluded`, `hidden`, `symlink`, `too_large`, `binary_no_extractor`, `unsupported_format`, `worker_absent`, `worker_incompatible`, `policy_refused`, `sandbox_required` (r7, §2.16) or `collision`. No item disappears from the report.
9. **Exclusions and symlink confinement.** The caller passes include and exclude globs. Built-in exclusions are hidden segments (`policy.rs`), VCS directories and the bundle root. Import canonicalizes the source root once and never follows a link below it. A link is reported as `symlink` and is not read. The bundle side keeps §2.11.
10. **An interrupted import recovers by re-running.** The engine writes the concept before its ledger line and syncs each ledger line (DOC-55 §2.2). A re-run therefore converges. Import appends one start line and one finish line per run under `_sources/imports/<run-id>.jsonl`. A start line with no finish line marks the run as interrupted in `import status`. Progress itself comes from the ledger (DOC-55 §6.3). Leftover temporary files from an atomic write are removed at the next `Bundle::open`.
11. **No automatic human verification.** Import never writes `verified`. A source field that looks like an approval, for example a document's "approved" property, stays an extension key and never maps to `verified`. r4: a source field whose key is itself protected, for example `verified` or `status`, is quarantined under `source_fields` (§2.13.1). `generated.by` is `process:trusty-okf-import`. The trust label comes from the store registration (§2.4), and defaults to `UntrustedExternal` (`okg/trust.rs`).
12. **The report keeps states apart.** Appendix A defines the report. Each item has one read outcome (read, skipped or failed), one write outcome (created, updated, unchanged, tombstoned or failed), one index state and one verification state (§4.9). A report says "fully searchable" only when every live concept the import touched is Searchable. A failed or skipped index push makes the report say so.

### 2.15 Email and Google Docs import (proposal; required scope) {#SPEC-OKF-01-2-15}

Scope is approved (§1.3). The rules below are proposals. Appendix C gives the proposed per-source matrix.

**Who owns what.**

- trusty-okf owns the common import interface (§2.13), normalization into concepts, provenance, IDs, the ledger, tombstones, the three revision states and the import report.
- Authenticated acquisition stays in source adapters. The Gmail and Drive adapters already live in trusty-agents (`crates/trusty-agents/src/tools/okg/gmail.rs`, `drive.rs`, `gapi.rs@c1eed98441`). They use the Google Workspace `BaseClient` (DOC-55 §3.3) and the credential authority (#4040). Boundary rules B1–B8 stay as written (§2.7.1).
- When a person uses the trusty-okf CLI, each authenticated source runs as a separate adapter program, one per source kind. The CLI starts it with the caller-supplied environment list and speaks the worker protocol in an `acquire` role (§3 (m), decided A; §2.7.1 B10). r7.1: each `acquire` request carries the run's `operator_initiated` value from `ImportSpec` (Appendix B). An adapter that receives a request with no `operator_initiated` field treats the run as not operator-initiated and applies SENT-only.

**Credentials.** trusty-okf receives no credential, token or account secret (B6, B10). The adapter process holds its own grant under its own principal (ADR-0026).

**Sub-agent imports. — Decided 2026-11-03 (owner): owner-choices item 3 A.** A sub-agent gets the in-process tier only: UTF-8 text and Markdown from a local tree (§2.16 "Tiers"). It starts no extraction worker and no adapter process, so ADR-0024 stays unchanged. trusty-okf knows nothing about agents (B3), so the caller states the limit. The `ImportSpec` field `child_processes: false` makes the run start no child process (proposal). Under it, an item that needs the worker is skipped with `policy_refused`, and a run over an authenticated source is refused with `policy_refused` before any write. trusty-agents sets the field for every sub-agent run. Criterion 144 tests this.

**Inbound email. — Decided 2026-11-03 (owner): owner-choices item 2 C.**

- SENT-only binds the automated OKG refresh: every run of the scheduled refresh runner (#4538) and every OKG build that no operator started. In those runs, the `gmail` adapter adds `in:sent` to the listing query, and a store query cannot remove it.
- An operator may start an explicit import with a named query that reads inbound mail. The run must be operator-initiated, and the CLI or caller marks it so. The mark reaches the adapter as `operator_initiated: true` in the `acquire` request (r7.1, Appendix B). A sub-agent cannot start one (item 3 A).
- Such an import is allowed only after the capability-grant review that DOC-63 requires for a new source type (Q2 resolution, `docs/specs/DOC-63-okg-sources.md:1655-1660@c1eed98441`). DOC-63 places that review after Phase A, so inbound import also waits for S-4.4. Until the review passes, the adapter refuses an inbound query.
- Every inbound concept carries the trust label `UntrustedExternal`.
- DOC-63 S-3.3 is amended to state this scope (§6.1 item 13; owner item 2 C; [#10742](https://github.com/bobmatnyc/trusty-tools/issues/10742), merged in [#10810](https://github.com/bobmatnyc/trusty-tools/pull/10810)).

**Native-Doc export. — Decided 2026-11-03 (owner): owner-choices item 10 B.** Native Google Doc export is a new source type, not the existing `drive` source. It goes through the capability-grant review before any import against a real account. DOC-63 places that review after Phase A. Option A, treating `gdocs` as the existing `drive` source, was not chosen.

**Proposed acceptance for these sources (not owner-approved).**

1. **Explicit selection.** The caller names the account label and the source: a Gmail query and window, or a Drive folder ID or document IDs. No default account exists. A missing selection is refused.
2. **Complete pagination.** The adapter follows every page token to the end. A run that stops at a ceiling reports `pagination_exhausted: false` and the remaining cursor. It never enables deletion detection (C4). r6: the engine sets `pagination_exhausted` and `deletion_safe` itself, whatever the adapter's flag says (§2.13 "Listing completeness"). Today `gmail_list_ids` returns at the ceiling with no truncation flag (`gapi.rs:135-137@c1eed98441`). The tool reports `listed` and `max_messages`, so a caller can infer truncation but is not told (`gmail.rs:224-232`).
3. **Immutable IDs, revisions and dates.** Gmail: the message ID is the item ID and its own revision (`gapi.rs:228-233`). Drive: the file ID is the item ID, and `version`, else `modifiedTime`, is the revision (`gapi.rs:396-407`). Each concept keeps the source dates: Gmail `internalDate` (`gapi.rs:191-199`) and the `Date` header; Drive `modifiedTime` and `createdTime`. The Drive listing does not request `createdTime` today (`gapi.rs:36`).
4. **IDs come from immutable source IDs, never from subjects.** Today a Gmail item's name is `<date> <subject>` (`gapi.rs:221-229`), which `slugify` turns into the path. Two same-day messages with one subject would then share a path (inference). Import uses `mail/<source-id>/<thread-id>/<message-id>` and `docs/<source-id>/<file-id>`.
5. **Incremental import and dedup.** The ledger skip test decides (DOC-55 C2–C3). Widening a window fetches only the delta (DOC-63 S-3.5).
6. **Email thread context.** Each message keeps `thread_id` (`gapi.rs:208-210`). Messages of one thread share a directory. A thread concept lists its messages in date order with bundle-absolute links (proposal).
7. **Docs structure and export fidelity.** A native Google Doc is exported, never downloaded (DOC-55 §4.6). The proposed export order is `text/markdown`, then `text/html`. Headings, lists, tables and links survive. Today `drive_content` returns `None` for a base64 payload (`gapi.rs:362-366`), so native exports that arrive as binary yield nothing. The import reports that case as `unsupported_format` instead.
8. **Format failures and deletions are reported.** Each failed export, extraction or fetch is a per-item failure with a reason. A deletion is reported as a tombstone only when the run is `deletion_safe` (§2.13, r6). A Gmail store is never deletion-safe, so Gmail never tombstones (DOC-55 C4). Drive `trashed` items count as deleted on a deletion-safe recursive listing.
9. **Resumability.** A re-run with the same selection resumes from the ledger and the last cursor. A crash costs at most one chunk of re-fetch (DOC-55 §2.2).
10. **States stay distinct.** The import report gives saved, index and verification counts separately (§4.9, Appendix A).
11. **No silent summary.** Import never stores a summary or snippet in place of a body unless the content policy asks for one. Today `message_to_item` falls back to Gmail's `snippet` when no text part decodes (`gapi.rs:211-216`). Import reports that case as `degraded: snippet_only`. Under the default policy (3k, decided A), it writes no concept for the item. A per-store policy that allows snippets writes the snippet, still flagged `degraded`.
12. **No whole raw mailbox by default.** The default never copies raw MIME, every attachment, or every label. The default policy (3k, decided A) copies the full body text of each selected message or document, with thread context. Quoted and forwarded text stays, marked as quoted. Attachments are listed by name, type and size, and not extracted. For an automated refresh, a selected message is a SENT message. An operator-initiated import after the grant review may also select inbound messages (3l, decided C).

### 2.16 The extraction worker (proposal; design approved) {#SPEC-OKF-01-2-16}

The design choice is approved (§1.4). Every rule below is a proposal. Appendix B gives the protocol messages and default limits.

**Shape.** The worker is a separate binary, proposed name `trusty-okf-xberg-worker`. It depends on xberg at an exact version (`xberg = "=1.3.10"`; Amended 2026-11-05, #10746 D19, Bob: "Amend §2.16 to =1.3.10." The assessment named 1.4.0 at `xberg-assessment.md:579`) with a committed lockfile. trusty-okf implements one `Extractor`, `WorkerExtractor`, that forwards each item to the worker. trusty-okf never links xberg (B9). The worker is optional: trusty-okf builds and imports without it.

**Tiers. — Decided 2026-11-03 (owner): §3 (o) option A; revisitable at implementation.** The built-in tier handles UTF-8 text and Markdown, which is today's floor (DOC-55 §4.3). Every other format goes to the worker. Not chosen: 3o B (pure-Rust extractors also in process) and 3o C (3o B through a shared crate with trusty-search).

**Process isolation is not a sandbox.** The worker runs as the same user. Without an OS sandbox it can read any file and open any socket that user can (Xberg assessment `:269`). The limits below reduce exposure. They do not confine the worker.

**OS sandbox. — Decided 2026-11-03 (owner): owner-choices item 9 B.** The worker runs under an OS sandbox before it parses email or Drive content. The sandbox allows no network and no file access beyond the worker's empty working directory. The mechanism is per platform, for example Linux landlock with seccomp, or the macOS sandbox (proposal). r7.1: the host applies the sandbox in its own spawn path, before the worker binary runs, and records whether that step succeeded. That record is the host's spawn evidence, and only it decides whether the worker is sandboxed. The host never trusts the worker to report it, as for the limits below. The worker's `hello` reply reports the sandbox in the `sandbox` field (Appendix B), which the host uses only as a cross-check. When the field disagrees with the spawn evidence, the run fails with `worker_incompatible`. The host sends an item from a `gmail`, `drive` or `gdocs` store only to a worker whose spawn evidence shows the sandbox. Otherwise the item is skipped with `sandbox_required`. Local-tree extraction may ship before the sandbox. Option A, sandbox as later hardening, was not chosen. Criterion 145 tests this.

**Limits.** The host enforces every limit. It never trusts the worker to.

| Limit | Default (proposal) | Enforcement |
|---|---|---|
| Input bytes per item | 8 MiB, matching `docstore` `MAX_FILE_BYTES` (`docstore.rs:40`) | Checked before sending. Over the limit: `too_large`, not sent. |
| Output text per item | 5 MiB, matching trusty-search `MAX_EXTRACTED_TEXT_BYTES` (Xberg assessment `:39`) | The host stops reading at the cap, kills the worker and fails the item with `output_too_large`. |
| Output fields and notes | 64 KiB of fields, 100 notes | Same as output text. |
| Wall time per item | 30 s, matching trusty-search `EXTRACT_TIMEOUT` (Xberg assessment `:40`) | A host timer. On expiry: kill, reap, fail the item with `timeout`, start a fresh worker for the next item. |
| Items per worker process | 500 | The host recycles the worker after 500 items to bound leaks. |
| Frame size | Input limit plus 64 KiB; output limit plus 64 KiB | A larger frame is a protocol error. |
| stderr capture | 64 KiB | Kept for the report as a bounded excerpt. Never written into the bundle (DOC-55 E4). |
| Handshake (r4) | 10 s from spawn to a valid `hello` reply | A host timer. It covers writing the host `hello`, reading the reply, and version negotiation. On expiry: kill, reap, `handshake_timeout`. |
| Shutdown (r4) | 5 s from `shutdown` to process exit | A host timer. On expiry: the kill sequence below, then `shutdown_timeout`. |
| Reap (r4) | 5 s from SIGKILL to a reaped child | A host timer. On expiry: `reap_timeout`; a reaper thread takes over the wait. |
| CPU and memory | `RLIMIT_CPU` and `RLIMIT_AS` where the OS honours them | macOS may not enforce `RLIMIT_AS` (inference). The wall-time limit is the guaranteed bound. |

The caller's item ceiling bounds the count per run, as `MAX_*_CEILING` does today (DOC-55 §2.2).

**Cancellation, kill and reap.** The host starts the worker in its own process group. To stop it, the host closes stdin, sends SIGTERM to the group, waits 2 s, sends SIGKILL to the group, then waits on the child under the reap deadline (r4). A `Drop` guard on the worker handle runs the same sequence, so a host error or panic leaks no process. Every spawned child is waited on, so none stays a zombie. A cancelled import marks the in-flight item as failed with `cancelled`. Its earlier items stay committed.

**Local-only configuration.** The host starts the worker with a cleared environment plus an allow-list (`LANG`, `TZ=UTC`). This rule binds the extraction worker. An acquisition adapter process (3m A) gets the caller-supplied environment list instead (§2.7.1 B10, r6). Its working directory is a fresh empty temporary directory. The worker receives bytes on stdin and never a path, so the input side matches DOC-55 E1. The worker is built without any xberg network or model-download feature. A reviewer checks this with `cargo tree -e features` on the worker. The worker refuses a request that asks for a feature it lacks. No network use is stated, so none is allowed. OCR and ML layout models stay off (§3 (p), decided A; revisitable at implementation).

**Handshake and version mismatch.** The first exchange is `hello`. The host sends its supported protocol range. The worker answers with one protocol version, its own version, the xberg version, a configuration hash, its formats and whether its output is deterministic. With no common protocol version, the host kills the worker and marks every item that needs it as `worker_incompatible`. The import continues for other items. It never falls back silently to a different extractor.

**Deadlines on every wait (r4).** r4 fixes review BLOCKER 7. What must be true: no host wait on the worker is unbounded. Every read and every write of a worker pipe runs under a deadline. Each item's frames run under the per-item wall time. The handshake, version negotiation and shutdown each run under their own deadline in the limits table.

- **Handshake.** The handshake deadline starts at spawn. It covers the host's `hello` write, which can block when the worker does not read stdin. It also covers the reply read.
- **Version negotiation.** Negotiation is the `hello` round trip plus the host's check of the reply. It has no further message. A first frame that is not `hello` is a `protocol_error`. A reply after the deadline is ignored.
- **On handshake expiry.** The host runs the kill sequence and reaps the worker. Every item that needs the worker fails with `handshake_timeout`, and the run records `WorkerError::HandshakeTimeout`. The host does not retry the spawn in the same run. Other items still import. The CLI exits with code 3 (§4.10).
- **Shutdown.** The host sends `shutdown`, closes stdin and waits for exit under the shutdown deadline. On expiry it runs the kill sequence and records `WorkerError::ShutdownTimeout` in the report. Committed items keep their outcomes.
- **Reap.** After SIGKILL the host waits for the child under the reap deadline. A process in an uninterruptible kernel wait can outlive it. Then the host records `WorkerError::ReapTimeout` and hands the child to a reaper thread. The import returns; only the reaper thread keeps waiting.

`WorkerError` has the variants `HandshakeTimeout`, `ShutdownTimeout`, `ReapTimeout`, `Incompatible` and `Spawn` (§4.4). Criteria 121–124 test this.

**Absent worker.** The host finds the worker only at a configured path, or next to the trusty-okf binary. It does not search `$PATH`, so another binary on `$PATH` cannot stand in for it. With no worker, items that need it are skipped with `worker_absent`. The plan shows this before any write. The report is not complete, and the CLI exits with the "completed with skips" code (§4.10).

**Provenance and errors.** Each extracted concept records `extracted_by: xberg-worker@<worker version>`, `extract_protocol`, `xberg_version`, `extract_config` (the configuration hash) and the input SHA-256. The worker echoes the input SHA-256, and the host checks it. The fingerprint tag is `sha256:<hex>/xberg-worker:<protocol>.<worker version>.<config hash>` (DOC-55 §4.5). So a worker upgrade or a configuration change re-extracts exactly the items it produced. Errors are per item and never end the run (DOC-55 E3). Appendix B lists the error codes.

**Determinism.** DOC-55 E1 needs byte-identical output for identical bytes. With OCR and ML models off, the worker must meet E1. A fixture test checks it. A worker whose handshake reports non-deterministic output is refused. 3p is decided A, so no determinism exception exists.

**Fixtures.** Fixtures are generated in the repo, so their licence is known. Appendix B lists them: text PDF, PDF with a table, scanned PDF, corrupt PDF, DOCX, XLSX, PPTX, a zip-bomb DOCX, an oversized file, and test-double workers that hang, crash, flood, lie about the hash, speak the wrong protocol and spawn a child that ignores SIGTERM.

**Changes this needs in DOC-55 (proposal; DOC-55 lives in trusty-tools).**

- §8 item 3, "no shelling out to an external converter" (`okg-universal-importer.md:603-606`): amend to allow exactly the versioned extraction worker under these rules.
- §3.3, "No OCR" (`:132-135`): unchanged, because 3p is decided A. The rejected 3p B would have amended it.
- E1, purity: the worker meets it on the input side (bytes only) and by the determinism test. It does not meet it on the process side, because a process can do I/O. The OS sandbox (item 9 B) bounds that I/O for email and Drive content. Local-tree extraction before the sandbox keeps this residual risk.
- r7: the amendment also states the OS sandbox condition above. The filing checks whether DOC-55 §8 item 3 also covers the 3m A adapter processes, which are connectors and not converters (inference).

**Reconciliation with the Xberg assessment (ar-T05).**

| Assessment item (`docs/briefs/2026-11-03-xberg-assessment.md`) | r3 disposition |
|---|---|
| Recommendation C, no addition now (`:5`) | Superseded for import by the 03:17Z owner choice. |
| "OKF does NOT own extraction (stays in trusty-agents connectors)" (`:171`) | Superseded by §1.2–§1.4. trusty-okf owns the extraction call for imports. It still owns no authenticated acquisition. |
| In-process plugin first (`:314`, `:483`) | Not adopted. Bob chose the separate worker. B9 forbids linking xberg. |
| MSRV 1.98 versus 1.94 is a blocker for in-process use (`:22`) | The process boundary removes it for trusty-okf. The worker builds in an excluded trusty-tools directory with its own toolchain (3n, decided B). |
| `DocumentExtractor` trait in trusty-common and a plugin registry in trusty-search (`:105-160`) | Not adopted. DOC-55's `Extractor` is the one contract. Whether trusty-search later uses the same worker is outside this spec. |
| TOML manifest with a `[permissions]` table (`:176-212`) | The limits and `network = false` shape is reused for worker configuration. The permission fields enforce nothing (`:224-225`), so this spec does not call them permissions. |
| Timeouts: `spawn_blocking` cannot be stopped (`:18`, `:273`) | Adopted. Timeouts kill the process. |
| Fixtures (`:507-510`) | Adopted and extended in Appendix B. |
| Out-of-process adapter deferred to v2 (`:453`) | Moved to the first milestone for trusty-okf. |
| Process isolation is not a sandbox (`:269`) | r7: an OS sandbox is required before the worker parses email or Drive content (item 9 B). |
| Acceptance routes `POST /indexes/...` (`:523`) | Stale. HTTP was retired in #9214 (§5.1). |

The brief also names "ar-Tff" for reconciliation. This spec found no document with that identifier, so it reconciles only ar-T05.

## 3. Owner decisions (all decided) {#SPEC-OKF-01-3}

Each question lists its options. Bob decided (a) at 03:06Z and every other question at 06:58Z on 2026-11-03, each as recommended (§1.5). The chosen option is marked **Chosen**. Every other option stays visible, marked "Not chosen", or "Ruled out by r6" where r6 found it incompatible with a requirement. Questions (f), (h), (n), (o) and (p) are owner-choices items 19–26: decided, and revisitable at implementation without reopening acceptance. The four approved inputs in §1.1–§1.4 are requirements, not §3 questions.

**Noted consumer (owner FYI 2026-11-03 04:13Z).** Trusty Agents' assistant knowledge store will use trusty-okf. This records a consumer only. It decides no §3 question and changes no requirement.

**(a) Crate name and ADR-0041. — Decided 2026-11-03 (owner): A.** Proposal: [ADR-OKF-0001](../adr/0075-trusty-okf-consolidates-trusty-kb-and-owns-the-index-feed-loop.md) (filed as ADR-0075).
- A. trusty-okf replaces the rename. trusty-kb's generic code moves into trusty-okf, its agent glue moves to trusty-agents, and trusty-kb retires. A new ADR amends ADR-0041's naming. **Chosen.**
- B. trusty-okf is the format crate. trusty-okg is the agent build crate on top (stores, ledger, ingest, trust labels, feed).
- C. Keep ADR-0041 and drop trusty-okf.
- Ruling: "consolidate generic trusty-kb functionality under trusty-okf, superseding planned trusty-okg naming; preserve agent glue boundary" (Bob, 2026-11-03 03:06Z). §2.7 gives the mapping and the boundary rule.

**(b) OKF version target. — Decided 2026-11-03 (owner): B.** Owner-choices item 13.
- A. v0.2 only. Not chosen.
- B. Write v0.2. Read v0.1 through the §13 fallbacks, and migrate an OKG by rebuilding it. **Chosen.**
- C. Write both v0.1 and v0.2. Not chosen.
- Reason: OKF §13 permits the fallback at low cost. The OKG is a build product, so a rebuild is the migration.
- Note: B is lossless only under the revised §4.5, which never deletes a concept the ledger does not own (§2.9). §2.8 applies to every migration path.
- Note (r4, r7): B regenerates a legacy concept only after it proves engine ownership, by the §3 (q) C proof. `migrate` covers hand-owned concepts (§2.9).

**(c) Supersession and correction model. — Decided 2026-11-03 (owner): A.** Owner-choices item 12.
- A. Deprecate in place, add `superseded_by` and `supersedes` keys, record in `log.md` (§2.5). **Chosen.**
- B. The Google sample pattern: move the old concept to a `-legacy` path, and the new concept takes the old ID. Not chosen.
- C. Replace in place. History lives only in `log.md` and git. Not chosen.
- Reason: A keeps every concept ID stable, so ledger and search-index paths stay valid. The keys make supersession machine-readable, which OKF v0.2 lacks.
- Sub-question (c2): a store item changes after a curator deprecated its concept. **Decided 2026-11-03 (owner): (i).** Owner-choices item 11, listed there as A. (i) Ingest writes the new source content and keeps the editorial keys. **Chosen.** (ii) Ingest refuses with `Conflict` and reports the item on every run. Not chosen. Reason: the tree follows the source, and §2.9 keeps the curator's decision. §4.5 and §4.8 apply (i).

**(d) Where trust and verification fields live. — Decided 2026-11-03 (owner): C.** Owner-choices item 5.
- A. OKF frontmatter only. Ruled out by r6 as written: the whole record in frontmatter breaks the bounded projection of §2.10 and the 64 KiB tombstone bound of §4.8, unless the overflow lives outside frontmatter, which makes it option C.
- B. A sidecar ledger only. Not chosen.
- C. Both. The record outside frontmatter is authoritative. The build projects a bounded copy into frontmatter. **Chosen.**
- Reason: a rebuild regenerates frontmatter, so human `verified` events must survive outside it. Only events in the record count as Verified; D64 (#10873) adds hashless frontmatter-only events to `history` only (§2.10 "Events with no hash"). Frontmatter keeps each bundle self-describing per OKF §5.2. §2.10 places the record.
- Note (r6): under C, `verify` changes concept bytes, so it records `written_bytes` (§4.5).

**(e) Owner of the feed loop to trusty-search. — Decided 2026-11-03 (owner): A.** Owner-choices item 14.
- A. trusty-okf, in a feature-gated module moved from `trusty-agents/src/stores/index_feed.rs`. **Chosen.**
- B. trusty-agents keeps it. Not chosen.
- C. trusty-search absorbs OKF indexing, per ADR-0041 Decision 2. Not chosen.
- Reason: the loop is generic, and ADR-0041 already wants it out of trusty-agents. C depends on trusty-search work that is not built (§5.5).
- Note: ADR-0041 Decision 2 still stands after ADR-OKF-0001. A contradicts it for the feed loop, so a further ADR-0041 amendment is a pre-implementation filing (§6.1 item 9). §5 binds trusty-okf as the feed owner.

**(f) Default strictness on write. — Decided 2026-11-03 (owner): B; revisitable at implementation.** Owner-choices item 23.
- A. Lenient: report findings, block nothing. Not chosen.
- B. Strict on errors: block any write that breaks OKF §11 conformance; report warnings and infos. **Chosen.**
- C. Strict on errors and warnings. Not chosen.
- Reason: a non-conformant write must never land. OKF §11 calls everything else soft guidance.

**(g) Public surface. — Decided 2026-11-03 (owner): B.** Owner-choices item 15. ADR-0040 sets the rule: agent-consumed tools go to MCP, framework-consumed tools stay native.
- A. Library only. Not chosen.
- B. Library plus a CLI: `validate`, `regen`, `build`, `migrate` and `feed` (3e is A), plus the `import` verbs that §1.2 item 16 requires (§4.10). **Chosen.**
- C. Library, CLI and an MCP server. Not chosen. It conflicts with ADR-0041 Decision 4 and boundary rule B7.
- Reason: the CLI gives validation, migration, rebuild and import a native, framework-consumed entry point; ADR-0041 notes that none exists today. ADR-0041 puts agent-facing reads in a trusty-mcp-services service, not here.
- Note (r3, r7): §1.2 item 16 and §1.3 item 23 require an import CLI. Under B it is the trusty-okf CLI. Its authenticated sources run as adapter processes (3m A).
- Note (r6, r7): the adapter environment list of §2.7.1 B10 stays a library parameter. The trusty-okf CLI supplies it, and any other host binary supplies its own.

**(h) crates.io publishing. — Decided 2026-11-03 (owner): A; revisitable at implementation.** Owner-choices item 24.
- A. `publish = false` now. **Chosen.**
- B. Publish to crates.io now. Not chosen.
- Reason: OKF is at v0.2 and still changing, and publishing would pin the schema. trusty-kb withheld publishing for this reason (`crates/trusty-kb/Cargo.toml:17-36@c1eed98441`). §7 is decided A, so nothing forces publishing.

**(i) Deprecated concepts in the search index. — Decided 2026-11-03 (owner): A.** Owner-choices item 16.
- A. Withdraw deprecated and tombstoned concepts from the search index. Re-check on every feed pass. **Chosen.**
- B. Keep them indexed. Not chosen.
- Reason: hits carry no metadata, so a caller cannot filter deprecated concepts out (§5.5).
- Note: trusty-search recognizes tombstones (`ingest/mod.rs:425`) but not `status: deprecated`. Its watcher re-indexes changed files under the index root (DOC-63 S-8.1), and a full reindex walks the whole root. Either path can return a withdrawn deprecated concept to the index (inference). So every feed pass re-checks withdrawn paths and removes any that came back (§5.4 F9). A trusty-search that learns `status: deprecated` would only make F9 rarer.

**(j) Where the crate lives. — Decided 2026-11-03 (owner): A.** Owner-choices item 18. See §7.

Questions (k)–(p) are new in r3. They come from the four 2026-11-03 owner inputs (§1.1–§1.4).

**(k) Default content policy for email and Google Docs import. — Decided 2026-11-03 (owner): A.** Owner-choices item 17. Bob requires a configurable policy, never a silent summaries-only default and never a whole raw mailbox by default (§1.3). Every option below is a default; a caller can pick another option per store.
- A. Full body text of each selected message or document, with thread context. Quoted and forwarded text stays, marked as quoted. Attachments are listed by name, type and size, but not extracted. **Chosen.**
- B. Option A, plus attachment extraction through the extraction layer, under the same limits. Not chosen as the default.
- C. Full body text with quoted and forwarded text removed. Not chosen as the default.
- D. No default. An import refuses until the caller names a policy. Not chosen.
- Summaries-only is available only as an explicit per-store choice.
- Reason: A imports the real content Bob asked for. It keeps thread meaning, which quote-stripping loses; DOC-63 calls quote-stripping "lossy and itself unreliable" (`DOC-63-okg-sources.md:834-838@c1eed98441`). It leaves third-party attachment bytes out by default (DOC-63 §6.4 item 2, `:840`).
- Note: for Gmail, a selected message is a SENT message in an automated refresh, and may be an inbound message in an operator-initiated import after the grant review (3l C).

**(l) Email scope versus the SENT-only ruling. — Decided 2026-11-03 (owner): C, as worded in the owner-choices list.** Owner-choices item 2. Bob's 03:15Z scope says "include email" and rejects summaries-only. The prior ruling makes Gmail import SENT-only, enforced at the query (DOC-63 §5.1 row b, S-3.3). DOC-63 also requires every new source type to be reviewed as a capability grant, after Phase A lands (Q2 resolution, `:1655-1660`). Phase A includes the chunk-level label carrier S-4.4, which is not built (§5.5).
- A. Keep SENT-only for every Gmail import. "Email import" means full-fidelity import of the SENT corpus. Not chosen.
- B. Add inbound mail as a new store kind, for example `gmail-inbox`. It is reviewed as a capability grant and stays blocked until Phase A lands. `gmail` stays SENT-only. Not chosen. r6 recommended B; the owner-choices list recommended C.
- C. SENT-only binds the automated OKG refresh only. An operator can run an explicit import with a named query that reads inbound mail. Such an import is labelled `UntrustedExternal` and is allowed only after the capability-grant review. **Chosen.** §2.15 "Inbound email" gives the rule.
- r6 option C, "Lift SENT-only for the `gmail` store kind" with no other condition, was replaced by the owner-choices wording above before the ruling. It is not chosen.
- Reason: C delivers inbound email, keeps the automated refresh SENT-only, and keeps the grant review DOC-63 requires.
- Note: C narrows a standing owner ruling, S-3.3. The narrowing is recorded with its date in §1.5, and DOC-63 S-3.3 carries the matching amendment (§6.1 item 13; owner item 2 C; [#10742](https://github.com/bobmatnyc/trusty-tools/issues/10742), merged in [#10810](https://github.com/bobmatnyc/trusty-tools/pull/10810)). DOC-63 places the grant review after Phase A, so inbound import still waits for S-4.4.
- Note: the shipped Gmail tool does not enforce SENT-only today. It accepts any Gmail query, and its schema offers `from:board@example.com` as an example (`crates/trusty-agents/src/tools/okg/gmail.rs:78@c1eed98441`). `gmail_query` adds only date operators (`gapi.rs:64-76`). Under C, the automated refresh must add `in:sent`. This is a gap to close in trusty-agents.

**(m) Where authenticated acquisition runs for the public import CLI. — Decided 2026-11-03 (owner): A.** Owner-choices item 4. §1.3 item 23 requires the public CLI to import email and Google Docs end to end. B1 and B6 keep connector code out of trusty-okf.
- A. Source adapter processes. trusty-agents (or a connector crate) ships one adapter binary per source kind. The trusty-okf CLI spawns it and speaks the same versioned process protocol as the extraction worker (§2.16), in an `acquire` role. The adapter holds its own grant from #4040. **Chosen.**
- B. Split CLI. The trusty-okf CLI imports local trees. `tagent okg import` imports email and Docs through the same trusty-okf library calls. Not chosen.
- C. trusty-okf links the Google adapters behind a cargo feature. Ruled out by r6: it links Google code into trusty-okf, so the B10 environment rule has no process boundary to bind, and it breaks B1 and B6.
- Reason: A gives one import CLI, one protocol for both process roles, and no B-rule change.
- Note (ADR-0024, settled): ADR-0024 says a sub-agent "never talks out-of-process" (`docs/adr/0024-…md:61-63@c1eed98441`), and DOC-63 reads it as "must not be given an out-of-process ingestion path" (`:1714`). The adapter processes and the extraction worker start child processes. **Decided 2026-11-03 (owner): owner-choices item 3 A.** A sub-agent gets the in-process tier only, so ADR-0024 stays unchanged (§2.15 "Sub-agent imports"). ADR-0026 applies: the process that acquires holds its own grant.
- Note (r6): A needs the B10 scope rule of §2.7.1, under which an adapter process gets exactly its caller's environment list.

**(n) Where the extraction worker crate lives. — Decided 2026-11-03 (owner): B; revisitable at implementation.** Owner-choices item 20. xberg needs Rust 1.98; the workspace pins 1.94 (ADR-0029; Xberg assessment `:22`).
- A. Its own repo, released as a binary. Not chosen.
- B. A directory in the trusty-tools repo that the workspace excludes (`[workspace] exclude`), with its own lockfile and `rust-toolchain.toml`. **Chosen.**
- C. A workspace member, after an ADR-0029 MSRV bump to 1.98. Not chosen.
- Reason: B keeps one repo and one PR for protocol changes, without moving the workspace MSRV.
- Note: B rests on an inference about Cargo `exclude` behaviour. Before implementation, a check confirms that the exclusion keeps the 1.94 workspace build clean, and ADR-0029 records the excluded directory (§6.1 item 15). If the check fails, 3n is revisited at implementation.
- Note: this question is separate from §7, which places trusty-okf itself.

**(o) Built-in extraction tier. — Decided 2026-11-03 (owner): A; revisitable at implementation.** Owner-choices item 25.
- A. trusty-okf handles UTF-8 text and Markdown in process. Every other format goes to the worker. Without the worker, other formats are reported `worker_absent`. **Chosen.**
- B. trusty-okf also runs DOC-55's first-wave pure-Rust extractors (`html`, `docx`, `xlsx`, `pdf`) in process, and the worker covers the rest. Not chosen.
- C. Option B, reusing trusty-search's extractors (`crates/trusty-search/src/core/extract/`, Xberg assessment `:26-48`) by moving them to a shared crate. Not chosen.
- Reason: A keeps one extraction path for binary formats and does not add a third set of PDF and Office parsers beside trusty-search's and xberg's.
- Note: Google Docs exported as HTML need an HTML-to-text step. That step runs in the worker, so a native-Doc import also needs the OS sandbox (§2.16).

**(p) OCR and ML models in the worker. — Decided 2026-11-03 (owner): A; revisitable at implementation.** Owner-choices item 26.
- A. Off. The worker builds without model download or OCR features. A scanned PDF yields empty text and a `no_text_layer` note, as DOC-55 §3.3 says today. **Chosen.**
- B. Opt-in per store, with models in a local directory the operator fills. No download at run time. Not chosen.
- Reason: DOC-55 §3.3 excludes OCR because it is neither pure nor deterministic (`okg-universal-importer.md:132-135`). B would need a DOC-55 amendment and a determinism exception.

Questions (q) and (r) are new in r4. They come from the review-blocker fixes, not from a new owner input.

**(q) How a legacy ledger-owned concept proves engine ownership. — Decided 2026-11-03 (owner): C.** Owner-choices item 6. r4 lets `rebuild` overwrite a ledger-owned concept only when stored proof shows the disk bytes are what the engine last wrote (§4.5). Records written before r4 carry no written hash (`crates/trusty-kb/src/okg/ledger.rs:47-65@c1eed98441`). Without a proof rule, 3b B ("migrate an OKG by rebuilding it") would regenerate no legacy concept.
- A. Equality proof. `rebuild` re-renders each legacy item with the trusty-kb v0.1 renderer. When the result equals the disk bytes, no hand edit exists, and `rebuild` records the hash. Every unequal concept stays preserved and reported. Not chosen alone.
- B. Operator adoption. An explicit `adopt` call, given a reviewed list of concept IDs, records their current bytes as engine-written. Not chosen alone.
- C. Both: A first, then B for the concepts A cannot prove. **Chosen.**
- Reason: A proves most generated concepts without a human step. B handles the remainder by explicit choice, never by default.
- Note (r6): the proof covers byte ownership only. Neither the equality proof nor `adopt` may give a hash to a legacy verification event (§2.10 "Events with no hash"). A trusty-okf write over unproven bytes records `proven: false`, so an ordinary write never creates proof (§4.5).

**(r) Where the machine-local index state lives. — Decided 2026-11-03 (owner): A.** Owner-choices item 7. §4.9.1 makes index state machine-local. That rule derives from §1.1. The location was the choice. B3 forbids trusty-okf to derive it from a home directory or an environment variable.
- A. The caller passes an index state directory. trusty-agents passes a directory in its machine-local data area, outside every per-store tree that ADR-0022 syncs. **Chosen.**
- B. trusty-search stores the journal beside its own index data and answers a state query. Not chosen. It would need a trusty-search change.
- C. A directory inside the bundle that every sync ignore list must exclude. Ruled out by r6: it contradicts §4.9.1 rule 1 and `StateDirInBundle`, and a synced copy would show another machine's state. The index identity of §4.9.2 does not remove that conflict.
- Reason: A keeps B3, needs no trusty-search change, and does not rely on each sync tool's ignore list.

## 4. API and lifecycle {#SPEC-OKF-01-4}

### 4.1 Module layout (proposal)

- `trusty_okf::format`: concept types, frontmatter, container rules, parse, render, validate, content hash. No I/O beyond files. No async.
- `trusty_okf::profile`: pluggable collection profiles and inverse-edge reconciliation.
- `trusty_okf::bundle`: open, create, put, move, verify, supersede, deprecate, `index.md` and `log.md` regeneration, paths, graph read, migrate. (Amended 2026-11-05, #10746 D14, Bob: `convert` dropped.)
- `trusty_okf::build`: registry, ledger, ingest, docstore fetcher, read policy, trust labels, index journal, rebuild.
- `trusty_okf::feed` (cargo feature `search-feed`): the push loop (3e, decided A).
- `trusty_okf::retrieve` (cargo feature `search-feed`): `search.query` plus hit enrichment. Bob's requirement 3 puts retrieval in trusty-okf.
- `trusty_okf::state` (r3): the revision journal and the three-state read model (§4.9). No I/O beyond bundle files.
- `trusty_okf::import` (r3): the common import interface, `plan_import`, `import`, the ID rule and the import report (§2.13–§2.15, §4.10). The `docstore` source adapter lives here or in `build::docstore`.
- `trusty_okf::extract` (r3): DOC-55's `Extractor` trait, type-hint resolution and the built-in text tier.
- `trusty_okf::extract::worker` (cargo feature `extract-worker`, r3): the worker client and protocol (§2.16).

§3 (a) = A fixes this split. §2.7.2 maps each existing module to it.

### 4.2 Types

```rust
pub struct ConceptId(String);            // validated per §2.11; bundle-relative path, no ".md"
pub struct ContentHash(String);          // "sha256:<hex>", per §2.10
// r4 (§2.2.1): `raw` is the source of truth; `frontmatter` is a read-only view of it.
pub struct Concept { pub id: ConceptRef, pub raw: RawFrontmatter, pub frontmatter: Frontmatter, pub body: String }
pub enum ConceptRef { Id(ConceptId), Foreign(PathBuf) }   // r4: Foreign = path fails §2.11; read-only
pub struct RawFrontmatter { pub text: String, pub entries: Vec<RawEntry> }   // r4: top-level entries in file order
pub struct RawEntry { pub key: String, pub span: std::ops::Range<usize>, pub value: serde_yaml::Value }
pub enum Field<T> { Absent, Valid(T), Malformed(serde_yaml::Value) }   // r4: a malformed value is kept, with a finding
pub struct Frontmatter {
    pub r#type: Field<String>,
    pub title: Field<String>, pub description: Field<String>,
    pub resource: Field<String>, pub tags: Field<Vec<String>>,
    pub sources: Field<Vec<Field<Source>>>, pub generated: Field<Generated>,
    pub verified: Field<Vec<Field<Verification>>>, pub status: Field<Status>,
    pub stale_after: Field<DateTime<FixedOffset>>,
    pub supersedes: Field<String>, pub superseded_by: Field<String>,
    pub extra: BTreeMap<String, serde_yaml::Value>,   // unknown and legacy keys, preserved
}
pub struct Source { pub resource: String, pub id: Option<String>, pub title: Option<String>, /* author, usage_count, last_modified */ }
pub struct Generated { pub by: Actor, pub at: Option<DateTime<FixedOffset>> }   // r4: OKF requires only `by`
pub struct Verification { pub by: Actor, pub at: DateTime<FixedOffset>, pub stale_after: Option<DateTime<FixedOffset>> }   // r7.1: `verify` only; a new `stale_after` set in the same write (§2.4), not projected into `verified[]`
pub struct VerificationEvent { pub v: Verification, pub hash: Option<ContentHash>, pub invalidated_at: Option<DateTime<FixedOffset>>, pub invalidated_by_hash: Option<ContentHash>, pub reason: Option<String> }   // #10746 D4b (2026-11-05, Bob): `hash` is `None` for a legacy event, which moves to history with `reason` `legacy_unbound` (§2.10)
pub enum Actor { Producer { name: String, version: String }, Human(String), Process(String), Other(String) }
pub enum Status { Draft, Stable, Deprecated, Other(String) }   // r4: an unknown value is kept
pub enum TrustTier { Unverified, MachineConfirmed, HumanReviewed }   // derived only
pub enum Staleness { Fresh, Stale, Unknown }
pub enum Severity { Error, Warning, Info }
pub struct Finding { pub severity: Severity, pub kind: FindingKind, pub concept: Option<ConceptId>, pub message: String, pub spec_ref: &'static str }
pub enum WriteMode { Lenient, StrictErrors, StrictWarnings }
pub enum PutOutcome { Created, Updated { content_changed: bool, verifications_cleared: usize }, Unchanged }
pub struct MigrationPlan { pub changes: Vec<FileChange> }
pub struct MigrationReport { pub run_id: String, pub changed: usize, pub findings: Vec<Finding> }
pub struct AdoptReport { pub adopted: Vec<ConceptId>, pub refused: Vec<(ConceptId, String)> }   // r7, 3q C (§4.5); r7.1: `refused` reasons per §4.5 "Adopt"
pub enum FeedStatus { Searchable, Unconfirmed, Partial, NotFed }   // §5.4; only Searchable is success (r3)
pub struct FeedReport { pub status: FeedStatus, pub acknowledged: usize, pub searchable: usize, pub removed: usize, pub pending: usize, pub errors: Vec<FeedError> }

// r3: state separation (§4.9)
pub struct RevisionState { pub id: ConceptId, pub revision: ContentHash, pub bytes: ByteHash, pub save: SaveState, pub index: IndexState, pub verification: VerificationState, pub okf_tier: TrustTier }
pub struct ByteHash(String);             // "sha256:<hex>" of the file bytes; keys the index state
pub enum SaveState { Saved, SavedExternal }
pub enum IndexState { NotIndexed, Pending { last_error: Option<String> }, Acknowledged, Searchable, WithdrawPending, Withdrawn }
pub enum VerificationState { Unverified { history: usize }, Verified { human: bool, events: usize, history: usize } }
pub struct IndexStateDir(PathBuf);       // r4 (§4.9.1): caller-supplied, machine-local, outside the bundle
pub struct IndexIdentity { pub instance_id: String, pub generation: u64 }   // r6 (§4.9.2): read from search.index.status (D1)
pub enum LiveIdentity { Known(IndexIdentity), Unsupported, Unreadable(String) }   // r6: what the caller read from the live daemon

// r4: operation journal (§4.11)
pub enum OpKind { Move, Supersede, Correct, Verify, Migrate, Rebuild }
pub enum RecoveryDirection { RollForward, RollBack }
pub struct RecoveryReport { pub op_id: String, pub kind: OpKind, pub direction: Option<RecoveryDirection>, pub conflicts: Vec<PathBuf> }
pub enum WorkerError { Spawn(String), HandshakeTimeout, Incompatible, ShutdownTimeout, ReapTimeout }   // r4, §2.16

// r3: import (§4.10, Appendix A)
pub struct ImportSpec { pub store: StoreSpec, pub include: Vec<String>, pub exclude: Vec<String>, pub max_items: usize, pub policy: ContentPolicy /* default: 3k A */, pub child_processes: bool /* r7: false for a sub-agent (§2.15) */, pub operator_initiated: bool /* r7: inbound Gmail needs true (§2.15) */ }
pub struct ImportPlan { pub run_id: String, pub items: Vec<PlannedItem>, pub pagination_exhausted: bool, pub deletion_safe: bool }   // r6 (§2.13): replaces complete_listing
pub struct ImportReport { /* Appendix A */ }
```

### 4.3 Functions

```rust
// format
impl ConceptId { pub fn parse(s: &str) -> Result<ConceptId, OkfError>; }      // §2.11
pub fn parse_concept(id: ConceptId, text: &str) -> Result<(Concept, Vec<Finding>), OkfError>;
pub fn render_concept(c: &Concept) -> String;                 // deterministic, "---" on line 1; a new concept renders key-sorted, an existing one is patched per §2.2.1 (r4)
pub fn content_hash(c: &Concept) -> ContentHash;              // §2.10
pub fn trust_tier(f: &Frontmatter) -> TrustTier;
pub fn staleness(f: &Frontmatter, now: DateTime<FixedOffset>) -> Staleness;

// bundle
impl Bundle {
    pub fn open(root: &Path) -> Result<(Bundle, Vec<Finding>), OkfError>;   // lenient read; root must exist; runs recovery first (r4, §4.11)
    pub fn recover(&mut self, op_id: &str, dir: RecoveryDirection) -> Result<RecoveryReport, OkfError>;   // r4: explicit choice for a conflicted operation
    pub fn create(root: &Path, okf_version: &str) -> Result<Bundle, OkfError>;
    pub fn validate(&self) -> Vec<Finding>;
    pub fn get(&self, id: &ConceptId) -> Result<Concept, OkfError>;
    pub fn put(&mut self, c: Concept, by: Actor, mode: WriteMode, now: DateTime<FixedOffset>) -> Result<(PutOutcome, Vec<Finding>), OkfError>;   // #10746 D4b (2026-11-05, Bob): returns the write's findings, so criterion 125's `legacy_value_moved` warnings have a channel
    pub fn move_concept(&mut self, from: &ConceptId, to: &ConceptId, now: DateTime<FixedOffset>) -> Result<MoveReport, OkfError>;
    pub fn verify(&mut self, id: &ConceptId, v: Verification, seen: &ContentHash) -> Result<(PutOutcome, Vec<Finding>), OkfError>;   // #10746 D4b (2026-11-05, Bob): returns findings as `put` does
    pub fn supersede(&mut self, old: &ConceptId, new: Concept, reason: &str, by: Actor, now: DateTime<FixedOffset>) -> Result<(), OkfError>;   // 3c A: deprecate old in place, create new (§2.5)
    pub fn adopt(&mut self, reviewed: &[(ConceptId, ByteHash)], by: Actor, confirmed: bool, now: DateTime<FixedOffset>) -> Result<AdoptReport, OkfError>;   // r7, 3q C: records reviewed bytes as engine-written; never hashes an event. r7.1: each pair names the bytes the person reviewed, as `verify` takes `seen`; `Human` actor and `confirmed` required (§4.5 "Adopt")
    pub fn deprecate(&mut self, id: &ConceptId, reason: &str, by: Actor, now: DateTime<FixedOffset>) -> Result<(), OkfError>;
    pub fn regenerate_indexes(&mut self) -> Result<IndexReport, OkfError>;
    pub fn plan_migration(&self) -> Result<MigrationPlan, OkfError>;        // writes nothing
    pub fn migrate(&mut self, plan: &MigrationPlan, now: DateTime<FixedOffset>) -> Result<MigrationReport, OkfError>;
}

// build
pub fn build(okg: &OkgSpec, stores: &[StoreSpec], items: &mut dyn ItemSource) -> Result<BuildReport, OkfError>;
pub fn rebuild(okg: &OkgSpec, stores: &[StoreSpec], items: &mut dyn ItemSource) -> Result<BuildReport, OkfError>;
pub fn index_backlog(okg: &OkgSpec, state: &IndexStateDir, index_id: &str) -> Result<IndexBacklog, OkfError>;       // pure; §5.4; r4 adds the machine-local state

// state (r3, §4.9; r4 adds the machine-local index state, §4.9.1)
impl Bundle {
    pub fn revision_state(&self, id: &ConceptId, state: &IndexStateDir, index_id: &str, live: &LiveIdentity) -> Result<RevisionState, OkfError>;   // read-only; detects SavedExternal; r6: checks `live` (§4.9.2)
    pub fn state_summary(&self, state: &IndexStateDir, index_id: &str, live: &LiveIdentity) -> Result<StateSummary, OkfError>;                    // counts per axis; never one "synced" flag; r6: checks `live`
}

// import (r3, §4.10)
pub trait ImportSource { /* DOC-55 Connector: list (cursor, complete flag) + fetch (§2.13) */ }
pub fn plan_import(bundle: &Bundle, spec: &ImportSpec, src: &mut dyn ImportSource) -> Result<ImportPlan, OkfError>;   // writes nothing
pub fn import(bundle: &mut Bundle, plan: &ImportPlan, src: &mut dyn ImportSource, ex: &dyn Extractor, now: DateTime<FixedOffset>) -> Result<ImportReport, OkfError>;

// feed (feature "search-feed"; 3e A)
pub async fn feed(okg: &OkgSpec, state: &IndexStateDir, index_id: &str, sink: &dyn IndexFeed) -> Result<FeedReport, OkfError>;   // r4: journal in `state`
// retrieve (feature "search-feed")
pub async fn retrieve(okg: &OkgSpec, index_id: &str, q: Query) -> Result<Vec<EnrichedHit>, OkfError>;
```

`OkgSpec` carries the bundle root path and nothing else that names an agent (B3). Every time value comes from a caller-supplied `now` or from source data, never from the system clock. trusty-kb's `put_entity` already takes `now` (survey §1).

### 4.4 Error contract

```rust
pub enum OkfError {
    Io { path: PathBuf, source: std::io::Error },
    Parse { path: PathBuf, line: Option<usize>, message: String },
    Conformance(Vec<Finding>),          // write refused under the active WriteMode
    Reserved(PathBuf),                  // concept named index.md or log.md
    InvalidId { input: String, rule: u8 },   // §2.11 lexical rule number
    PathEscape(PathBuf),                // path leaves the bundle root, or crosses a link
    NotFound(ConceptId),
    Conflict { id: ConceptId, reason: String },   // e.g. move target exists, stale verify hash
    MigrationLoss(Vec<LossItem>),       // §2.9 audit failure; every changed file restored
    RootMismatch { index_id: String, index_root: Option<PathBuf>, bundle_root: PathBuf },   // §5.4 F5
    JournalWrite { path: PathBuf, source: std::io::Error },   // §5.4 F3
    Search(trusty_common::search_rpc::SearchRpcError),        // feed and retrieve only
    SourceOverlap { source_root: PathBuf, bundle_root: PathBuf },   // r3, §2.14 rule 1
    SelectionRequired(String),          // r3, §2.15: no account or source named
    Worker(WorkerError),                // r3, §2.16; only for run-level failures, items fail individually
    RecoveryPending { op_id: String, paths: Vec<PathBuf> },   // r4, §4.11: an incomplete operation needs an explicit recover
    StateDirInBundle(PathBuf),          // r4, §4.9.1: index state must live outside the bundle
    IndexScope { index_id: String, leaked: Vec<PathBuf> },   // r6, §5.3.1: the index holds a `_sources/` or `_state/` path
    AdoptRefused { reason: String },    // r7.1, §4.5 "Adopt": `not_human` or `not_confirmed`; no file changes
}
```

- No public function panics on malformed input.
- A read never fails on a finding of any severity. Only I/O failure or unparseable YAML in the target concept fails a read.
- A refused write changes no file. Each file write is atomic: a `create_new` temporary file in the same directory, then a rename (§2.11). An operation that writes more than one file is atomic as a set through the operation journal (§4.11, r4).
- One writer works on a bundle at a time. trusty-okf takes an advisory lock file under `_sources/` (proposal). ADR-0022 sync must not commit it (§2.12). The lock is built in slice S5 of the #10746 plan. (Amended 2026-11-05, #10746 D4b, Bob: "lock goes in S5.")

### 4.5 Idempotency and rebuild guarantee {#SPEC-OKF-01-4-5}

A concept is **ledger-owned** when a ledger record names its path. Every other concept is **hand-owned**: a bundle `put`, a migrated file (#10746 D14: was "a converted file"), or a file a user created.

- `put` of identical content returns `Unchanged` and writes no bytes.
- `regenerate_indexes` run twice writes no bytes the second time.
- `build` with no store change writes no files and leaves the feed backlog empty.
- `rebuild` regenerates every ledger-owned concept from store contents and the ledger. It never deletes or rewrites a hand-owned concept, and it never deletes `_sources/` (§2.9).
- With equal inputs and no hand edits, `rebuild` yields a byte-identical tree at any wall-clock time.
- Editorial keys and the verification record survive a rebuild. Under the decided 3d C the record lives outside frontmatter, and `rebuild` re-projects the bounded frontmatter copy from it (§2.10).

**Rebuild invariants (r4).** r4 fixes review BLOCKER 2. r3 regenerated `log.md` from ledger events, which erased the deprecate, supersede, move, verify and correction entries that §2.3 appends. r3 also detected hand edits with a ledger field that legacy records lack.

What must be true:

1. `rebuild` removes, rewrites and reorders no existing `log.md` entry.
2. `rebuild` overwrites a ledger-owned concept only when stored proof shows the disk bytes are what the engine last wrote.
3. An interrupted `rebuild` leaves no concept file with partial or mixed content, and the next open completes it.

**`log.md`.** `rebuild` never regenerates `log.md`. It inserts its own entries, for example one `**Rebuilt**` entry per run, and nothing else changes. trusty-okf inserts a new entry directly below the newest date heading when that heading is the entry's date. Otherwise it adds a new heading at the top (OKF §9). So deleting the lines an operation reports as added yields the old file byte for byte. This rule binds every trusty-okf write to `log.md`, not only `rebuild`.

**Ownership proof.** r6 rewrites this rule (reconciliation row 2). In r5.1 only `build`, `rebuild` and `import` recorded proof. Under 3d C, `verify`, `deprecate` and a `stale_after` renewal also change a concept's bytes. The next `rebuild` then saw trusty-okf's own write as a hand edit and never refreshed the concept, and §2.14 rule 7 refused the re-import. A changed source then never reached any verified or deprecated concept.

What must be true: every trusty-okf write to a ledger-owned concept records `written_bytes`, the byte hash of the bytes it wrote. Only bytes that no trusty-okf operation wrote count as a hand edit.

- **Which writes record.** Every write that changes a ledger-owned concept's bytes: `build`, `rebuild`, `import`, a tombstone and its reversal, `verify` (3d C projects the event into frontmatter), `deprecate`, a `stale_after` renewal, `put`, `supersede`, a link rewrite by `move_concept`, `migrate` and every recovery step. The record is a ledger field, `written_bytes` (proposal), plus the revision-journal line (§4.9).
- **Proven or unproven.** A record is proven when it is a source-derived engine write, or when the bytes it replaced matched a proven record. A write over unproven bytes records `proven: false`. So no ordinary trusty-okf write turns a possible hand edit into proof. Only the §3 (q) C steps below create proof for legacy bytes. A legacy record with no `written_bytes` is unproven (`crates/trusty-kb/src/okg/ledger.rs:47-65@c1eed98441`).
- **Source or caller content.** A record has `origin: source` when its content hash equals the content hash of the newest source-derived write. A lifecycle-only write, such as `verify`, `deprecate`, a `stale_after` renewal or a tombstone, keeps `origin: source`. A write that changes the content for a caller has `origin: caller`. Examples are a content-changing `put`, a correction in place and a link rewrite.

`rebuild`, and a re-import under §2.14 rule 7, compare the disk byte hash with the newest record for the concept.

- **Equal, proven, `origin: source`.** The concept is engine-owned. `rebuild` may replace its content. It keeps the editorial keys and the verification record (§2.9), and §2.10 moves verification to history on a content change.
- **Equal, proven, `origin: caller`.** A trusty-okf caller changed the content. `rebuild` leaves the concept byte-identical and reports `Conflict { reason: "caller_edit" }`. This is not a hand edit.
- **Equal, unproven, or no record.** The concept counts as possibly hand-edited. `rebuild` leaves it byte-identical and reports `Conflict { reason: "no_written_hash" }`. Such a concept gains proof by §3 (q) C. First `rebuild` tries the equality proof: it re-renders the item with the trusty-kb v0.1 renderer, and on equal bytes it records a proven `written_bytes`. Then an operator may `adopt` the reviewed bytes of the concepts that remain ("Adopt" below). Neither step gives a hash to a verification event (§2.10).
- **Different.** The concept is hand-edited. `rebuild` leaves it byte-identical and reports `Conflict { reason: "hand_edit" }`.

**Adopt (r7.1, proposal).** `adopt` binds proof to the bytes a person reviewed, as `verify` binds an event to `seen` (§4.3). Each entry is a `(ConceptId, ByteHash)` pair: the concept and the byte hash of the file the person reviewed. So a file edited between the review and the call is never recorded as engine-written, and the next `rebuild` cannot overwrite that edit.

- The call requires `by` to be `Actor::Human` and `confirmed` to be true. The caller sets `confirmed` only from an explicit human confirmation, as for a `human:` verification (§2.4). Otherwise the call returns `OkfError::AdoptRefused` with reason `not_human` or `not_confirmed`, and no file or record changes. A process actor cannot adopt, so adoption stays an explicit choice (§3 (q) C).
- For each pair, `adopt` records a proven `written_bytes` with `origin: source` only when the disk byte hash equals the given hash. Otherwise it lists the concept in `AdoptReport.refused` with one reason:
  - `byte_hash_mismatch`: the disk bytes differ from the reviewed bytes.
  - `not_found`: no concept has that ID.
  - `not_ledger_owned`: no store's item ledger lists the concept, so `rebuild` never writes it.
  - `already_proven`: the newest record is already proven, so `adopt` leaves it unchanged.
- A refused pair changes nothing. Other pairs in the same call still proceed.

A deprecated concept whose source item changed follows sub-question (c2), decided (i): it counts as engine-owned like any other. The import writes the new source content and keeps `status: deprecated` and the other editorial keys. The rejected (c2)(ii) would have refused it with `Conflict { reason: "deprecated" }`. Criteria 130 and 131 test this rule.

A `Conflict` on one concept does not stop the run. The other concepts rebuild, and `BuildReport` lists every preserved concept. The pre-image check at swap time (below, §4.11) preserves a hand edit made during the run too.

**Staged generation and swap.** `rebuild` writes nothing into the tree while it generates.

1. **Generate.** It renders every regenerated concept into a staging area, `_sources/ops/<op-id>/new/`, mirroring bundle paths. Concepts whose output equals the disk bytes are dropped from the set.
2. **Intent.** It writes an operation-journal intent record of kind `Rebuild` (§4.11). The record lists each target path with its pre-image byte hash and its staged post-image byte hash. The staged `log.md` post-image is one of the targets.
3. **Swap.** Immediately before each rename, it compares the target's disk byte hash with the pre-image hash in the intent record (§4.11). On a match, it renames the staged file over the target. Each rename is atomic. On a mismatch, it stops and leaves the operation as `recovery_conflict` (§4.11). The check covers `log.md` too.
4. **Complete.** It renames the staged `log.md` into place last, after the same check, writes the completion record and removes the staging area.

No single file system call replaces a set of files. The set is atomic through the journal: an interrupted swap rolls forward on the next `Bundle::open` (§4.11). A crash before the intent record leaves the tree unchanged, and the next open removes the staging area.

Criteria 28, 110, 111 and 112 test this section.

### 4.6 Bundle lifecycle

| State | Entered by | Meaning |
|---|---|---|
| Absent | — | No root directory. |
| Created | `create` | Root `index.md` with `okf_version: "0.2"`. |
| Dirty | any write, `build` with changes, `migrate`, `import` | At least one concept's index state is not Searchable or Withdrawn (§4.9). |
| Synced | a feed pass with `FeedStatus::Searchable` | Every live concept's current file bytes are Searchable, and every withdrawn path is Withdrawn (§4.9). r6: trusty-okf computes Synced at each read from the checked index states of §4.9.2. It never stores Synced as a flag. |
| Rebuilt | `rebuild` | Ledger-owned concepts regenerated; returns to Dirty, then Synced after a feed pass. |

A feed pass that ends `Unconfirmed`, `Partial` or `NotFed` leaves the bundle Dirty (§5.4). r3 change: r2 entered Synced on journal completion alone. Under §1.1 item 13 an acknowledgement is not retrieval proof, so Synced now needs the retrieval probe (§4.9). "Synced" is a bundle-level claim. It says nothing about verification.

### 4.7 Concept lifecycle

This table is the status axis. It says what a concept is. The three revision states (§4.9) say whether the current revision is saved, searchable and verified. They are separate axes. "In search index" below is the intended index content; the index state says whether it got there.

| State | Entered by | Stored as | In search index |
|---|---|---|---|
| Draft | `put` with `status: draft` | `status: draft` | Yes |
| Stable | `put` (default) | `status` absent or `stable` | Yes |
| Verified | `verify` | new `verified[]` event bound to the content hash; tier is derived. r3: this row is the verification axis of §4.9, not a status. | Yes |
| Stale | time passes `stale_after` | derived, not stored | Yes |
| Corrected | `put` that changes the content hash | new body and `generated`; `verified[]` cleared to the record (§2.10); `log.md` entry. The new revision is Unverified and Pending (§4.9). | Yes, re-pushed |
| Superseded | `supersede` | 3c A: `status: deprecated` and `superseded_by` on the old concept; `supersedes` on the new one (§4.8) | Old: No (3i A, withdrawn). New: Yes |
| Deprecated | `deprecate` | `status: deprecated`, ID unchanged (3c A, §4.8) | No (3i A, withdrawn) |
| Tombstoned | source item deleted | `source_status: deleted`, `tombstoned`, body kept | No (§4.8) |
| Rebuilt | `rebuild` | ledger-owned only; byte-identical with equal inputs | Unchanged |

Renewing freshness is a `put` that sets a new `stale_after`. It is not a content change. On a human-reviewed concept only a person or a fresh `verify` may move `stale_after` later (§2.4, item 19 B).

### 4.8 Correction and tombstone lifecycle {#SPEC-OKF-01-4-8}

Two mechanisms retire content. They have different causes and must stay separate.

- A **tombstone** is source-driven. A store item vanished. The engine marks the concept, keeps the body, and appends a `Tombstoned` ledger record (`crates/trusty-kb/src/okg/ingest.rs:319-358@c1eed98441`). It is reversible: when the item reappears, a re-ingest clears the mark.
- A **deprecation** is editorial. A curator or a correction retires the concept. Under the decided 3c A it is `status: deprecated` in place, with `superseded_by` when a successor exists.

**Tombstone detection contract.** trusty-common's `is_tombstone` returns true only when all of these hold: line 1 is exactly `---`; the frontmatter is at most 64 KiB; it has top-level `source_status: deleted`; and it has a non-empty top-level `source_id` (`crates/trusty-common/src/knowledge_document.rs:10-37@c1eed98441`). trusty-search removes such a file on `file.put` and skips it on batch parse (`crates/trusty-search/src/core/indexer/ingest/mod.rs:425-436`, `:809-811`). trusty-okf's renderer must therefore write `---` as line 1 with no byte-order mark, and keep `source_id` and `source_status` at the top level (§2.8). trusty-okf never writes `source_status: deleted` on a concept without `source_id`, because trusty-search would not see it as a tombstone.

**Frontmatter bound (r6, reconciliation row 14).** `is_tombstone` returns false when the frontmatter passes 64 KiB (`crates/trusty-common/src/knowledge_document.rs:30@c1eed98441`). If verification history grew without bound in frontmatter, a long-lived concept's tombstone would read as a live document. The watcher would then re-index the deleted body, while the index state read Withdrawn.

What must be true: a rendered tombstone's frontmatter is at most 64 KiB, as `is_tombstone` measures it.

- Verification history in frontmatter is only the bounded projection of §2.10. A write that would cross the bound keeps the overflow in the out-of-frontmatter verification record, which the decided 3d C provides (§2.10 "Bounded frontmatter").
- Before it renames a tombstone into place, trusty-okf runs `is_tombstone` on the rendered bytes.
- When `is_tombstone` still returns false, for example because an out-of-band edit grew another key past the limit, the tombstone still lands. trusty-okf reports a `tombstone_undetectable` error finding. That path never reads Withdrawn: it reads WithdrawPending, and each feed pass removes it again (F9).

Criterion 141 tests this.

**Transitions outside supersession.**

| From | Event | To | File change | Ledger | What the search index holds after a successful feed pass |
|---|---|---|---|---|---|
| Live | store item vanishes and the store has `tombstone_deleted` set (`okg/registry.rs:114`) | Tombstoned | adds `source_status: deleted` and `tombstoned`; body and other keys kept | `Tombstoned` record | Nothing for this path |
| Tombstoned | store item reappears | Live | source-derived keys replaced; tombstone keys removed; editorial keys and verification record kept (§2.9) | `Ingested` record | The new content |
| Tombstoned | source content unchanged, re-ingest | Tombstoned | none | none | Nothing |
| Any | content hash changes | same state | §2.10 steps | per write path | The new content, unless the state is withdrawn |
| Live, hand-owned | user wants it gone | Deprecated | never a tombstone; never a deletion | none | Nothing for this path (3i A) |

A tombstone does not change the content hash, so it keeps `verified[]` (§2.10). A tombstoned concept that is also deprecated stays withdrawn, because both the tombstone and 3i A withdraw it.

**Transitions by 3c option.** **Decided 2026-11-03 (owner): 3c A.** The 3c B and 3c C columns are kept for reference only. Neither applies.

| Event | 3c A (deprecate in place) — **Chosen** | 3c B (`-legacy` move) — Not chosen | 3c C (replace in place) — Not chosen |
|---|---|---|---|
| Deprecate concept X | X gets `status: deprecated`. ID unchanged. | X gets `status: deprecated`. ID unchanged. | X gets `status: deprecated`. ID unchanged. |
| Supersede X by Y | X gets `status: deprecated` and `superseded_by: /Y.md`. Y is created with `supersedes: /X.md`. | X's content moves to `X-legacy` with `status: deprecated`. Y's content is written at X's path. `move_concept` rules apply to the `-legacy` path. | X's content is replaced by Y's. No second file. History is `log.md` and git only. |
| Index after the feed pass | X withdrawn (3i A). Y pushed. | X's path holds Y's content. `X-legacy` withdrawn under 3i A. | X's path holds the new content. |
| Verification of X | kept; status change is not a content change | moves with the old content to `X-legacy` | cleared to the record; the content changed |
| Store item for X changes later | (c2)(i): the import writes the new source content and keeps `status: deprecated`, `superseded_by` and the other editorial keys. X stays withdrawn. | (c2), applied to the path the ledger names | ordinary re-ingest |
| Caller content edit (`put`) of a deprecated concept | refused with `Conflict` (deprecated is terminal for a caller edit). A source change follows the row above. | refused with `Conflict` | not applicable |

Under 3c B, the ledger names X's path, so the ledger item would own Y's content (inference). That was one cost of option B.

### 4.9 State separation model {#SPEC-OKF-01-4-9}

**Basis.** §1.1 is approved: saved, indexed/searchable and verified are tracked independently for the current revision. The model below is the proposal that implements it.

**Unit.** State is kept per concept, per revision. A revision is a content hash (§2.10). The index state is keyed by the byte hash of the current file, because the feed sends file bytes and those bytes include lifecycle keys. So a lifecycle-only write (status, `stale_after`, a tombstone) keeps the revision and its verification, but makes the index state Pending.

**Axis 1: save state.**

- **Saved.** The file on disk holds the bytes that trusty-okf last recorded for the concept.
- **SavedExternal.** The file on disk differs from the last recorded bytes, or no record exists. Causes: a hand edit, an ADR-0022 sync merge, another tool, or a concept written before r3.
- A failed write is not a state of the current revision. A refused or failed write leaves the previous revision current (§4.4). The operation's report lists the failed write.

Storage (proposal): a revision journal at `_sources/revisions.jsonl`. Each trusty-okf write appends one line before its rename: concept ID, content hash, byte hash, operation, `now` and run ID. On read, a line counts only when the disk bytes match its byte hash. So a crash between the journal line and the rename never marks unwritten bytes as Saved. The journal is append-only and torn-line tolerant, like the ledger (`okg/jsonl.rs`).

**Axis 2: index state.**

| State | Meaning |
|---|---|
| NotIndexed | No push has been attempted for this path. |
| Pending | A backlog task exists for the current bytes, or r6: a confirmation exists but cannot be bound to the live index (§4.9.2). It carries the last error, if any. |
| Acknowledged | The daemon confirmed a `file.put` of exactly these bytes with `indexed: true` (§5.4 F4), and the index journal recorded it. |
| Searchable | Acknowledged, and the retrieval probe passed for these bytes. |
| WithdrawPending | The concept should not be in the index (tombstone, or deprecation under the decided 3i A), and withdrawal is not confirmed. |
| Withdrawn | The daemon confirmed the remove, and the withdrawal probe found no hit for the path. |

An acknowledgement alone never reaches Searchable (§1.1 item 13). The evidence is direct: under #3892 a push succeeded, the chunks were in the corpus, and search returned nothing (DOC-55 §7.2 item 6, `okg-universal-importer.md:571-576@c1eed98441`).

**The retrieval probe (proposal; r6 revision evidence).** After an acknowledged put, the probe runs two checks. Both must pass.

1. **Retrieval.** The feed sends one `search.query` scoped by `path_prefix` to the concept's path, in lexical mode. The query text is the first 12 words of the body, or the title when the body is empty. The check passes when a hit at that path returns content that occurs in the current file.
2. **Revision evidence (r6, reconciliation row 4).** Check 1 alone can pass on text that the current and a prior revision share. After an edit lower in the file, chunks of the old revision would satisfy it. What must be true: the probe passes only on evidence specific to the current bytes. The feed uses one of two tests:
   - **Chunk coverage.** `search.chunks.list` for the path returns chunks whose text each occurs verbatim in the current file. Together the matched spans cover every non-whitespace character of the current body. A chunk whose text does not occur in the current file is a prior-revision chunk, and the test fails.
   - **Content hash, once D1 exists.** `search.index.file.status` for the path reports a `content_sha256` equal to the current byte hash, under the live index identity (§4.9.2, §5.6).

   Text that both revisions share is never evidence. trusty-okf keeps no prior-revision bytes, so it does not use a distinguishing-text test. Chunk coverage assumes the chunker returns verbatim spans of the file (inference; verify against trusty-search's Markdown chunker before implementation). If it does not, the test fails, and the state stays Acknowledged. That fails closed.

A stale-chunk hit fails the probe, so F6 is also caught here. The withdrawal probe passes when the same scoped query and `search.chunks.list` for the path return nothing. Probe results go into the index journal as their own record kind, with the index identity they were observed against (§4.9.2). After an explicit `search.index.reindex` that leaves the index identity unchanged, the feed demotes every Searchable path to Acknowledged and probes again on the next pass. A reindex that changes the identity makes those paths Pending (§4.9.2).

**Axis 3: verification state.**

- **Verified.** At least one non-invalidated event in the verification record carries the current content hash. The state reports whether any such event has a `human:` actor.
- **Unverified.** No event carries the current content hash. The state reports how many history events exist, including hashless frontmatter-only events (D64, #10873).
- Events bound to other hashes are history. They never count toward the current revision (§1.1 item 12).
- r6: an event with no hash is history too. No operation gives it a hash (§2.10 "Events with no hash").
- The verification record lives outside frontmatter and is authoritative (§3 (d), decided C; §2.10). Axis 3 reads the record for Verified and for `VerificationMismatch`. Ruling D64 (#10873): for the `history` count it also reads frontmatter `verified[]` entries that have no `hash` key and whose `(by, at)` no folded record line shares. Such an event is history, never Verified, and never a mismatch (§2.10 "Events with no hash").

**OKF trust tier, kept distinct.** The OKF trust tier stays as OKF §5.3 defines it: derived from frontmatter `verified[]`, advisory, and visible to every OKF consumer. trusty-okf reports it beside the verification state, never in place of it. The two agree while trusty-okf maintains the frontmatter. After an out-of-band edit they can differ: raw frontmatter may still show old events (§2.10 residual risk), while the verification state is Unverified. Any trusty surface that says "verified" uses the verification state.

**Reporting rule.** No surface reports "done", "synced", "imported" or "verified" from one axis on behalf of another. `state_summary`, `FeedReport`, `ImportReport` and the CLI give each axis its own count.

**How each operation moves the three states.**

| Operation | Save | Index (keyed by bytes) | Verification (keyed by revision) |
|---|---|---|---|
| `put`, content change | Saved (new revision) | Pending | Unverified; old events become history (§2.10) |
| `put`, lifecycle-only change | Saved (same revision, new bytes) | Pending | Unchanged |
| `put`, no change | Unchanged | Unchanged | Unchanged |
| `put` refused or failed | Unchanged; failure reported | Unchanged | Unchanged |
| `verify`, seen hash matches | Saved (3d C projects the event into frontmatter) | Pending when the bytes changed | Verified |
| `verify`, stale hash | Unchanged (`Conflict`, §2.10) | Unchanged | Unchanged |
| Out-of-band edit | SavedExternal | Pending (bytes differ from the journal) | Unverified when the content hash changed (§2.10); warning finding |
| `import`, new item | Saved | Pending | Unverified, with no history |
| `import`, changed item | Saved | Pending | Unverified; old events become history |
| `import`, unchanged item | Unchanged | Unchanged; a pending task is still pushed (DOC-55 §7.2 item 4) | Unchanged |
| `import`, item skipped or failed | No concept written; reported | Unchanged | Unchanged |
| `import` or `build` with a failed index push | Saved | Pending, with the error | Unchanged by the push |
| `rebuild`, equal inputs | Unchanged (byte-identical, §4.5) | Unchanged | Unchanged |
| `rebuild`, changed inputs | Saved | Pending | Unverified when the content hash changed |
| `migrate` | Saved | Pending (bytes changed) | Unchanged; a migration keeps the content hash (§2.10, criterion 34) |
| Correct in place | As `put`, content change | Pending | Unverified; history kept |
| `deprecate` | Saved | WithdrawPending (3i A) | Unchanged |
| `supersede` (3c A) | Old: as `deprecate`. New: as `put`, new item | Old: as `deprecate`. New: Pending | Old: unchanged. New: Unverified |
| Tombstone | Saved | WithdrawPending | Unchanged (§4.8) |
| Tombstone reversed | Saved | Pending | Unverified when the content hash changed |
| `move_concept` | Saved at the new path | New path Pending; old path WithdrawPending | Moved concept: unchanged, because the path is not in the hash. Each concept with a rewritten link: Unverified (§2.10 Moves) |
| Feed pass, put confirmed | Unchanged | Acknowledged | Unchanged |
| Feed pass, probe passed | Unchanged | Searchable (or Withdrawn) | Unchanged |
| Explicit `reindex` | Unchanged | Searchable demoted to Acknowledged; Pending when the index identity changed (§4.9.2) | Unchanged |
| Index deleted, re-created or reset (r6) | Unchanged | Pending (Acknowledged, Searchable); WithdrawPending (Withdrawn) | Unchanged |

**r2 failure cases in this model.** §5.4 F1–F11 keep their behaviour. Each now names its index state.

| r2 case | Index state after the pass |
|---|---|
| F1 daemon down, F8 unknown index | Pending for every task |
| F2 partial batch, F7 remove then failed put | Acknowledged for confirmed tasks; Pending for the rest |
| F3 journal append failed | Pending, never Acknowledged |
| F4 `indexed: false` | Pending, with the reply reason as the error |
| F5 root mismatch | Unchanged; the pass refuses before any push |
| F6 stale chunks | Acknowledged until a probe passes; a stale hit fails the probe |
| F9 withdrawn path reappears | Withdrawn becomes WithdrawPending |
| F10 unreadable file | Pending, with the error |
| F11 torn journal line | Pending; the task re-enters the backlog |
| F12 (r3) put confirmed, probe fails | Acknowledged; the pass returns `Unconfirmed` |
| F13 (r3) probe cannot run | Acknowledged; the pass returns `Unconfirmed` |
| F14 (r6) index identity changed | Pending for every bound record; WithdrawPending for every Withdrawn record |
| F15 (r6) index identity unsupported or unreadable | Pending at read; the pass returns at best `Unconfirmed` |

#### 4.9.1 Index state is machine-local (r4) {#SPEC-OKF-01-4-9-1}

r4 fixes review BLOCKER 5.

**Derivation.** §1.1 is approved. Item 11 makes a failed index update read as pending-index, and item 13 says an acknowledgement alone is not retrieval proof. Bob's confirmation adds that a failed or absent search update must not imply "done". An acknowledgement and a probe therefore describe the daemon that answered them. Each machine runs its own trusty-search daemon. So the index state that one machine reports must come from its own daemon. This rule follows from §1.1; it is not a new proposal.

**The defect in r3.** The index journal was `_sources/<id>.index.jsonl`, inside the bundle. ADR-0022 syncs the whole tree, `_sources/*.jsonl` included, through a per-store git repo with union merge (`docs/adr/0022-knowledge-tree-sync-model.md:87-90`, `:108@c1eed98441`). Machine B then pulled machine A's journal and read Acknowledged or Searchable for content its own daemon never received.

**What must be true.**

1. The index journal, its probe records and every derived index state live outside the bundle root and outside every synced path. That includes `_sources/`.
2. Each journal belongs to one machine, one bundle root and one index ID. One file holds it: `<index-state-dir>/<sha256 of the canonical bundle root>/<index-id>.jsonl` (proposal).
3. A machine whose journal has no record for the current bytes reads the concept as Pending. A second machine that receives a bundle by sync therefore starts with every concept Pending, until its own daemon confirms each push and probe.
4. trusty-okf never reads a journal under the bundle root as index state. A legacy `_sources/<id>.index.jsonl` stays in place, unchanged (§2.9 item 8). `Bundle::open` reports it with an info finding.

**Location.** The caller passes the index state directory as an `IndexStateDir` (§4.2). B3 forbids trusty-okf to derive it from a home directory or an environment variable. §3 (r) is decided A: the caller passes a directory outside the synced store tree. trusty-agents passes one in its machine-local data area. trusty-okf refuses an `IndexStateDir` inside the bundle root with `StateDirInBundle`.

**What stays in the bundle.** The revision journal `_sources/revisions.jsonl` (save state) and the verification record stay with the bundle. They describe the files, not a daemon, so a synced copy reports them correctly.

Criteria 118 and 119 test this section.

#### 4.9.2 Index state is bound to an index identity (r6) {#SPEC-OKF-01-4-9-2}

r6 resolves reconciliation row 5. The rule derives from §1.1 items 11 and 13, as §4.9.1 does.

**The defect in r5.1.** Only an explicit `reindex` demoted Searchable. A corpus reset, a migration fault (`crates/trusty-search/src/service/server/status.rs:328-342@c1eed98441`), or a delete and re-create of the same index ID left this machine's journal saying Searchable. The bundle then read Synced while the index held nothing. `search.index.status` exposes no generation or creation identity at `c1eed98441`: its reply names the index ID, roots, chunk count, migration faults and stages, and nothing that changes when the index is re-created (`status.rs:414-440`). So the journal has nothing to compare.

**What must be true.**

1. Each Acknowledged, Searchable or Withdrawn record in the index journal names the index identity it was observed against. The identity is the pair `(instance_id, generation)` from dependency D1 (§5.6).
2. Every `revision_state`, every `state_summary` and every feed pass compares each record's identity with the live index identity. The caller reads the live identity from `search.index.status` and passes it as `LiveIdentity` (§4.2). So the core stays network-free (B8).
3. On a mismatch, an Acknowledged or Searchable record reads Pending, and a Withdrawn record reads WithdrawPending. The next backlog lists a push or remove task for each path.
4. When the live identity cannot be read (`LiveIdentity::Unreadable`: daemon down, status error or unknown index), each such record reads Pending or WithdrawPending. The record stays in the journal. A later read with a matching identity reads it as recorded.
5. Until D1 exists (`LiveIdentity::Unsupported`: the status reply carries no identity), a record carries no identity, and it reads Pending with `last_error: "index_identity_unsupported"`. The feed still pushes and probes. A pass whose pushes and probes all succeed returns `Unconfirmed`, never `Searchable`. So no surface can report Searchable or Synced before D1 exists. The backlog does not re-push a path whose journaled byte hash matches, so a pass over an unchanged bundle pushes nothing. Once D1 exists, a record with no identity never matches a known identity, so rule 3 re-pushes it.
6. Bundle-level Synced (§4.6) is computed from these checked states at each read, never from a stored flag.

**3r and the identity.** 3r is decided A, so the journal lives in the caller's index state directory, and each record carries the D1 identity. The ruled-out 3r C would keep index state inside the bundle, which §4.9.1 rule 1 and `StateDirInBundle` forbid. The rejected 3r B would have moved the journal into trusty-search.

Criteria 134 and 135 test this section.

### 4.10 Import lifecycle and CLI {#SPEC-OKF-01-4-10}

Scope is approved (§1.2, §1.3). The phases, CLI shape and exit codes are proposals.

**Phases.**

1. **Plan.** `plan_import` lists the store, assigns IDs, picks extractors and records every refusal. It writes nothing (§2.14 rule 6).
2. **Apply.** `import` registers the store with `upsert`, then ingests in chunks. For each item it appends the revision-journal line, renames the concept file into place, then appends the ledger line (§4.9; DOC-55 §2.2 entity-before-ledger order).
3. **Feed.** A feed pass pushes the backlog and runs the probes (§5.4). trusty-okf runs it (3e A), with `--feed` or a separate `feed` call. An import without a feed pass leaves every touched concept Pending, and its report says so.
4. **Report.** Appendix A defines the report.

**CLI (3g B; authenticated sources run as adapter processes, 3m A).**

```
trusty-okf import plan   <bundle-root> --from dir:<path> [--include G] [--exclude G] [--json]
trusty-okf import run    <bundle-root> --from dir:<path> [--include G] [--exclude G] [--feed --index <id>] [--worker <path>]
trusty-okf import run    <bundle-root> --from gmail  --account <label> --query <q> [--after D] [--before D] [--policy P] [--operator]   # 3k A default; 3l C
trusty-okf import run    <bundle-root> --from gdocs  --account <label> (--folder <id> [--recursive] | --doc <id>...)    # 3k A default; grant review first (item 10 B)
trusty-okf import status <bundle-root> [--store <id>] [--json]
```

`--dry-run` on `import run` equals `import plan`. `--index` is the caller's index ID; trusty-okf never names an index (B5). `--feed` runs a feed pass after the import (3e A). `--operator` marks the run operator-initiated, which an inbound Gmail query needs after the grant review (§2.15 "Inbound email"). Without `--operator`, the `gmail` adapter keeps `in:sent` in the query. `import status` prints per-axis counts per store and lists interrupted runs.

**Exit codes (proposal).**

| Code | Meaning |
|---|---|
| 0 | Every planned item is saved or unchanged. With `--feed`, every touched live concept is Searchable. |
| 1 | Run-level error after writes began. Committed items stay committed. |
| 2 | Refused before any write: bad arguments, `SourceOverlap`, `SelectionRequired`, `RootMismatch`, a run-level `policy_refused` (r7, §2.15). |
| 3 | Completed with skipped or failed items: unsupported format, `worker_absent`, `sandbox_required` (r7), `collision`, extraction or fetch failures. |
| 4 | Saved, but not fully searchable: some touched concept is Pending or Acknowledged. |

Code 2 excludes the others, because nothing was written. Otherwise, when several codes apply, the CLI returns the first that applies in the order 1, 3, 4. The JSON report lists all of them.

### 4.11 Operation journal and multi-file crash recovery (r4) {#SPEC-OKF-01-4-11}

r4 fixes review BLOCKER 1 (C2). r3 made each file write atomic, but several operations write more than one file. A crash between two of those writes left a mixed state with no recovery rule.

**What must be true.** A crash at any point in a multi-file operation leaves a bundle that the next `Bundle::open` restores to one of two states: the state before the operation, or the state after it. No verification event is lost. No link points at a missing concept.

**Scope.** The journal covers every operation that writes more than one file:

- `move_concept`: the new file, every concept with a rewritten link, the ledger and verification-record lines, `log.md`, and removal of the old path.
- `supersede` (3c A): both concepts and `log.md`.
- Correct in place, and any content-changing `put` (3d C): the concept file and the verification-record line.
- `verify` (3d C): the verification-record line and its frontmatter projection.
- `migrate` (§2.9) and the `rebuild` swap (§4.5).

A single-file write keeps the §4.4 rule and the revision journal (criterion 66).

**Location.** The journal is `_sources/ops/journal.jsonl`. Each operation's staging files sit in `_sources/ops/<op-id>/`. They stay with the bundle, because recovery acts on the bundle's own files. They stay out of sync commits (§2.12). The bundle lock (§4.4) is held from the first staging write to the completion record, and during recovery.

**Staged names (r6, reconciliation row 1).** Every file under `_sources/ops/<op-id>/new/`, `old/` and `conflict/` carries the suffix `.okf-staged`, appended to its full name: `notes/a.md` stages as `notes/a.md.okf-staged`. trusty-search's walker indexes no such extension (`crates/trusty-search/src/service/walker.rs:24-40@c1eed98441`). Migration backup blobs (`sha256-<hex>`, §2.9) carry no extension. So a staged or conflict copy stays out of the index even when an index binding misses the §5.3.1 exclusions. Recovery strips the suffix when it renames a staged file into place. Paths elsewhere in this spec name staged files without the suffix.

**Records.** The journal is append-only and torn-line tolerant (`okg/jsonl.rs`). Each record is synced to disk before the next step starts.

- **Intent.** Written before the first write to any bundle file. It holds the op ID, the `OpKind`, the caller's `now`, the actor, and the ordered step list.
- **Steps.** A file step names a path, its pre-image byte hash (or `absent`) and its post-image byte hash (or `absent`, for a removal). An append step names a ledger, verification-record or revision-journal file and the exact line. Each appended line carries the op ID and its step index.
- **Completion.** Written after the last step. It holds the op ID and, after recovery, the direction taken.

**Before the intent.** The operation writes every post-image to `_sources/ops/<op-id>/new/` and a full copy of every existing target to `_sources/ops/<op-id>/old/`. A `migrate` uses its content-addressed backups (§2.9) as the `old/` copies. Every staged file is synced to disk before the intent record. So an intent record always names complete staged files.

**Step order.** Steps that add come before steps that remove. `move_concept` writes the new file, then the linking concepts, then the ledger and verification-record lines, then `log.md`, and removes the old path last. `supersede` (3c A) writes the new concept before it marks the old one. So at every step boundary, each verification event exists in at least one file, and each link written so far points at an existing concept.

**Pre-image check before each file step (r5).** Immediately before each file step, the operation compares the file's current disk byte hash with the step's recorded pre-image hash. The check runs in normal execution and in roll forward. It does not apply during roll back or `Bundle::recover` (r5.1), and `Bundle::recover` clears the operation's conflict marker when it completes. It covers every §4.11 operation, including the `rebuild` swap and its `log.md` step. On a match, the step runs. On a mismatch, the operation stops before the step and overwrites nothing. It writes no completion record. It reports a `recovery_conflict` finding that names the path, and the operation stays in the state of rule 2 below. A file that a person, another tool or a sync merge changed during the operation therefore keeps its bytes.

**Recovery on open.** `Bundle::open` takes the lock and reads the journal before it returns. For each intent with no completion record, it classifies each file step by the disk byte hash: at pre-image, at post-image, or other.

1. **Every step at pre or post.** Recovery takes the direction of the operation kind. `Migrate` rolls back, because its audit has not run: each file at post is restored from `old/`, and each file the migration created is removed. Every other kind rolls forward: each file at pre is replaced by its staged post-image, and each removal step runs. Roll forward appends each journal line that has no line with the same op ID and step index. Kind `Migrate` has no append steps, so roll back never needs to remove a line.
2. **Any step at other.** A file changed outside trusty-okf after the crash. Recovery changes no file. It reports a `recovery_conflict` finding that names the paths. Every write that touches a path of that operation is refused with `RecoveryPending`. `Bundle::recover(op_id, direction)` resolves the operation by explicit choice. It first copies each file at other to `_sources/ops/<op-id>/conflict/`, so the out-of-band edit survives.
3. **Finish.** Recovery writes the completion record with the direction taken, then removes the staging directory.

Recovery is idempotent. A crash during recovery leaves each step at pre or post, so the next open finishes it.

**Torn or missing intent.** An intent line that does not parse counts as absent. No bundle file was written, because writes start only after the intent record is synced. The next open removes the staging directory.

**Lock held elsewhere.** When another live writer holds the lock, `Bundle::open` does not recover. It returns an info finding `recovery_deferred`, and reads may see the mixed state until the writer finishes.

**Why no verification event is lost.** The events move by append steps that run before the removal step. Roll forward completes them. Roll back returns to files that still hold them. A conflict keeps every version in `old/`, `new/` and `conflict/`.

Criteria 106–109 and 126 test this section with fault injection.

## 5. Search boundary {#SPEC-OKF-01-5}

§5 binds the feed owner. §3 (e) is decided A, so the owner is trusty-okf's `feed` module. "The feed" below means that module.

### 5.1 Transport

trusty-search speaks JSON-RPC 2.0 over a Unix-domain socket. The HTTP listener was retired in [#9214](https://github.com/bobmatnyc/trusty-tools/issues/9214) (`crates/trusty-search/src/service/rpc/writes.rs:6@c1eed98441`). The client is `trusty_common::search_rpc`: `search_socket()`, `call_at` (`crates/trusty-common/src/search_rpc.rs:255@c1eed98441`) and `SearchRpcError`. Index-scoped calls use the envelope `{index_id, body: {...}}` (survey §3).

### 5.2 What the feed sends

| Method | Use |
|---|---|
| `search.index.status` | Preflight: confirm the search index root covers the bundle root. Results outside the root are filtered out (survey §3). r6: read the index identity (D1). |
| `search.index.config.get` (r7) | The admission filters for the §5.3.1 scope proof. Today it returns `extensions`, `exclude_globs`, `extra_skip_dirs` and `invalid_exclude_globs` (`crates/trusty-search/src/service/server/index_config.rs:51-76@c1eed98441`). D2 adds `include_paths` and `path_filter`, or a per-path admission query. |
| `search.index.file.put` | Push one concept, path relative to the index root. r6: once D1 exists, check the reply's `content_sha256` and `identity`. |
| `search.index.file.status` (r6, D1; not built) | Revision evidence for the probe (§4.9). |
| `search.index.file.remove` | Withdraw one concept. |
| `search.index.files.remove` | Withdraw a prefix or path list, for a moved directory or a rebuild. r6: also clears a scope leak (§5.3.1). |
| `search.index.reindex` | Only on an explicit `rebuild`, and only after the §5.3.1 scope proof. |
| `search.query`, `search.chunks.list` | r3: the retrieval and withdrawal probes (§4.9). r6: the scope check (§5.3.1). |

trusty-okf's `retrieve` sends `search.query`, scoped by `path_prefix` to the bundle. It then re-reads each hit's file to attach the concept ID, status, OKF trust tier, staleness and trust label. A hit it cannot read gets `UntrustedExternal`. Fence rendering stays in trusty-agents (`crates/trusty-agents/src/tools/memory/okg_fence.rs`).

r3 (proposal, from §1.1): a hit can carry text from an older revision. `retrieve` attaches the verification state only when the hit's content occurs in the current file. A historical review never makes stale text look verified.

**Stale-revision hits. — Decided 2026-11-03 (owner): owner-choices item 22 B; revisitable at implementation.** A hit whose content does not occur in the current file is returned, flagged `stale_revision` and reported as Unverified. A hit for a tombstoned concept, or for a concept whose index state is WithdrawPending or Withdrawn, is dropped. Option A, drop every stale-revision hit, was not chosen. Criterion 69 tests this.

The tree write is the truth. The ledger never waits on a push (DOC-55 SPEC-OKGIMPORT-06 §7.2.4). The index journal records a push only after the daemon confirms it (§5.4).

### 5.3 What the feed never does

- It never calls an HTTP route.
- It never creates or deletes a search index. trusty-agents owns the binding (`stores/binding.rs`) and the protected `assistant-okg-*` index creation (survey §1).
- It never pushes a file outside the bundle root, and never pushes a path under `_sources/` or `_state/`. It names those trees only to check for and remove a leak (§5.3.1).
- It never makes a tree write or ledger record wait on a push.
- It never chunks, embeds or ranks content.
- It never takes a trust label from a search hit.
- It never advances the index journal for a write the daemon did not confirm.

#### 5.3.1 What the index never holds (r6) {#SPEC-OKF-01-5-3-1}

r6 resolves reconciliation row 1. The trusty-search walker indexes `md` and `toml` files, and `SKIP_DIRS` has no `_sources` (`crates/trusty-search/src/service/walker.rs:24-40`, `:52-100@c1eed98441`). The daemon watcher sees every file under the root. So without this rule, `registry.toml` (docstore roots and Gmail queries) becomes searchable. So do the `old/` and `conflict/` copies of §4.11, which hold prior and out-of-band revisions. A deprecated, tombstoned or superseded body could then stay searchable through such a copy while its concept reads Withdrawn. "The feed never sends `_sources/`" binds only the feed, not the walker or the watcher.

**What must be true.** No file under `_sources/` or `_state/` is ever in the search index, through the feed, the watcher or a full reindex.

**How it holds.**

1. **The binding admits every path except `_sources/**` and `_state/**`.** trusty-search applies one admission policy to the reindex walk, the watcher, the rescan and pushed writes (`crates/trusty-search/src/service/index_admission.rs:7-12`, `:82-104@c1eed98441`). The caller creates the index (B5). r7.1: the binding states the rule as the literal `exclude_globs` entries `_sources/**` and `_state/**`, with no allowlist. A docstore import keeps the source hierarchy (§2.14 rules 2–3), so a concept ID can sit under any directory, and an allowlist of path prefixes cannot name them all. This binding also passes the rule 4 exclusion proof today. An `md`-only allowlist does not suffice, because `_state/conversion-log.md` is a `.md` file. The knowledge pipeline's `notes`-only allowlist (`include_paths: ["notes"]`, `crates/trusty-agents/src/api/server/knowledge_pipeline/indexing.rs:63@c1eed98441`) excludes both trees, but it also refuses pushes of every concept outside `notes/`, because `admits_pushed` enforces the configured roots (`write_admission.rs:64-100@c1eed98441`, as cited by the delta review of r6). That fails closed (F2), but an import bundle needs the wider admission. `_sources/**` also matches a nested bundle, because `path_matches_any_glob` tries `**/<pat>` (`repo_config.rs:384-390@c1eed98441`). This is dependency D3 (§5.6).
2. **Staged files carry a suffix that no walker indexes.** Every staged and conflict copy ends in `.okf-staged` (§4.11 "Staged names").
3. **Each feed pass checks scope first.** Before any push, the pass sends `search.chunks.list` with a `path_prefix` of the bundle's `_sources/` directory, and again with its `_state/` directory, each relative to the index root. When either returns a chunk, the pass removes the prefix with `search.index.files.remove`, pushes nothing, and returns `OkfError::IndexScope` with the leaked paths. The journal is unchanged, and no state advances.
4. **`rebuild` sends `search.index.reindex` only after a scope proof.** The proof shows that the index admits no path under either tree. r7 corrects the r6 premise. `search.index.config.get` already returns `extensions`, `exclude_globs`, `extra_skip_dirs` and `invalid_exclude_globs` (`crates/trusty-search/src/service/server/index_config.rs:51-76@c1eed98441`), and trusty-agents already calls it (`knowledge_pipeline/indexing.rs:86-90@c1eed98441`). So an exclusion proof is possible today: `exclude_globs` or `extra_skip_dirs` exclude both trees, and `invalid_exclude_globs` is empty. trusty-okf accepts only literal entries for the proof, `_sources/**` and `_state/**` as globs or `_sources` and `_state` as skip directories, so it interprets no other glob (proposal). An allowlist proof needs `include_paths` and `path_filter`, which the reply does not carry. That is dependency D2 (§5.6). Until D2 exists, `rebuild` sends `search.index.reindex` only on an exclusion proof. For a binding that relies on an allowlist, it relies on per-path pushes and removes instead.

Rule 3 detects a leak; it does not prevent one. Prevention rests on rules 1 and 2. A binding that misses rule 1 can expose `registry.toml` between two passes, and the next pass then refuses and removes it. Criteria 128 and 129 test this section.

### 5.4 Index reconciliation failures {#SPEC-OKF-01-5-4}

**Rule.** No failure path advances the index journal, and no failure path reports success. A feed pass returns `FeedStatus::Searchable` only when every task succeeded, the backlog is empty, every probe passed (§4.9), and r6: the live index identity is known and matches every record (§4.9.2). `Unconfirmed` means every put was acknowledged but some probe failed or could not run. `Unconfirmed`, `Partial` and `NotFed` are failures. The CLI exits non-zero on them. A caller must not log them as warnings while it reports the bundle as synced. r3 replaces r2's `Synced` with `Searchable` (§1.1 item 13).

**Journal coverage.** The backlog covers every concept in the bundle, not only ledger items. Today `okg_index_backlog` walks ledger records only (`crates/trusty-kb/src/okg/index_journal.rs:260-316@c1eed98441`). A hand-owned concept is never pushed, and a moved, superseded or hand-deleted path is never withdrawn. The revised backlog diffs two sets: the concept files on disk, and every path the journal records as indexed. A file with no current `Indexed` record at its hash is a push task. A journaled path with no file, or with a withdrawn state, is a remove task (proposal). r4: "the journal" is this machine's journal for the index ID, in the index state directory (§4.9.1).

**Per-task order.** For each task: optional remove of the old revision, then `file.put`, then reply check, then journal append. The journal line records the SHA-256 of the bytes actually sent. Today the journal records the hash taken when the backlog was computed, while the content is read later (`crates/trusty-agents/src/stores/index_feed.rs:256-260@c1eed98441`).

| # | Failure | Required behaviour |
|---|---|---|
| F1 | Daemon down: socket absent, connection refused or timeout | Tree writes and ledger records still land. The pass returns `NotFed` with the full backlog as `pending`. The journal is unchanged. The bundle stays Dirty. |
| F2 | Partial batch: some tasks fail | Each successful task is journaled on its own. Each failed task stays pending, with one error per task. The pass returns `Partial`. One failing concept does not stop the others, as today (`index_feed.rs:221-234`). |
| F3 | `file.put` confirmed, journal append fails | The task counts as failed, not indexed. The pass stops at once, because a failing journal write points at the disk (proposal). It returns `OkfError::JournalWrite`. Remaining tasks stay pending. The next pass re-pushes the task; a put is an upsert, so the re-push is safe. |
| F4 | `file.put` returns success with `indexed: false` (`empty_file`, `no_chunks`, `too_large`) | The task fails, and the journal is unchanged. trusty-search returns this reply as a normal result, not an error (`crates/trusty-search/src/service/server/files.rs:116-121@c1eed98441`). Today `RpcIndexFeed` ignores the reply body (`index_feed_rpc.rs:35-43`), so the journal records a refused write as indexed. The only zero-chunk success is a tombstone reply with `removed: true`. |
| F5 | Root mismatch: index root missing, not absolute, or not containing the bundle root | The pass refuses before any push and returns `RootMismatch`. The journal is unchanged. The trait's `Ok(None)` case, which skips the check today (`index_feed.rs:98-100`, `:200`), becomes a refusal. A root that changes mid-pass fails the remaining tasks; `RpcIndexFeed` re-reads the root on every call (`index_feed_rpc.rs:13-18`). |
| F6 | Stale chunks after a shorter revision | After the pass, `search.chunks.list` for the path returns only the new revision's chunks. trusty-search now removes a path's superseded chunk IDs inside `file.put` ([#8959](https://github.com/bobmatnyc/trusty-tools/issues/8959), `ingest/mod.rs:488-519`). The feed keeps remove-then-put until a test proves #8959 covers the relative-path key that the feed sends (inference). |
| F7 | Remove succeeds, put fails | The concept is unsearchable until the next pass. The journal still holds the old hash, so the next backlog lists a push task. The pass returns `Partial`. |
| F8 | Unknown index (`CODE_NOT_FOUND`, `crates/trusty-common/src/search_rpc.rs`) | The pass returns `NotFed`. The feed does not create the index (§5.3). |
| F9 | A withdrawn path reappears in the index (watcher or full reindex, §3 (i)) | Each pass re-checks withdrawn paths with `search.chunks.list` and removes any that came back. This covers deprecated and tombstoned concepts (§3 (i), decided A). |
| F10 | Concept file unreadable when the backlog is built | The task is reported as failed and stays pending, as the backlog note does today (`index_journal.rs:269-278`). |
| F11 | Torn journal line after a crash | The torn line is skipped and counted (`crates/trusty-kb/src/okg/jsonl.rs`). Its task re-enters the backlog and is re-pushed. |
| F12 (r3) | `file.put` confirmed, retrieval probe fails | The task is Acknowledged, not Searchable. The pass returns `Unconfirmed`. The next pass probes again. |
| F13 (r3) | `file.put` confirmed, probe cannot run (daemon gone after the put) | Same as F12. |
| F14 (r6) | The live index identity differs from a record's identity: the index was deleted and re-created, reset, restored or migrated with loss (§4.9.2) | Every Acknowledged and Searchable record reads Pending, and every Withdrawn record reads WithdrawPending. The pass re-pushes and re-withdraws each path. The bundle is not Synced. |
| F15 (r6) | The live identity is unsupported (no D1) or unreadable | Records read Pending or WithdrawPending (§4.9.2 rules 4–5). A pass whose pushes and probes all succeed returns `Unconfirmed`. |
| F16 (r6) | The index holds a path under `_sources/` or `_state/` | The pass removes the prefix, pushes nothing and returns `OkfError::IndexScope` (§5.3.1). |

### 5.5 Known gaps

- **No metadata channel.** A hit is a `CodeChunk` with fixed fields. Trust labels must be re-read from the file (`crates/trusty-agents/src/tools/memory/okg_fence.rs:19-30@c1eed98441`). DOC-63 S-4.4's chunk-payload label does not exist.
- **No status awareness.** trusty-search handles tombstones only. It does not know `status: deprecated` (§3 (i)).
- **ADR-0041 follow-ups are not built.** trusty-search has no reserved-filename handling, no frontmatter-as-metadata and no trust label in the chunk payload (survey headline 3).
- **Method constants.** `search_rpc` exports only seven method names. `file.put` and `file.remove` are string literals today (survey §3). trusty-search defines `METHOD_INDEX_FILE_PUT` in its own crate (`crates/trusty-search/src/service/rpc/writes.rs:100`). Adding constants to trusty-common is a follow-up.
- **Reindex scope.** r6 confirms the gap: a full `search.index.reindex` walks the whole root and does not skip `_sources/` (`crates/trusty-search/src/service/walker.rs:52-100@c1eed98441`). §5.3.1 closes it through the binding (D3) and gates `reindex` on a scope proof from `search.index.config.get`, which D2 completes for allowlists.
- **No index identity.** `search.index.status` reports no instance or generation identity (§4.9.2). D1 adds one (§5.6).
- **Stale HTTP text.** ADR-0041 still describes `POST /indexes/{id}/index-file` and `HttpIndexFeed` (`docs/adr/0041-…md:62,66,93@c1eed98441`). DOC-63 still names `HttpIndexFeed` (`docs/specs/DOC-63-okg-sources.md:564@c1eed98441`). Both routes were retired in #9214.
- **r1 correction.** r1 listed stale chunks as an open gap. #8959 closed it on the daemon side (F6).

### 5.6 External dependencies (r6) {#SPEC-OKF-01-5-6}

trusty-okf does not build these. Another crate owns each one. §6.1 item 11 requires each to be filed before implementation starts. Until a dependency exists, the stated fallback holds, and no fallback reports a false success.

| ID | Owner | What trusty-okf needs | Until it exists |
|---|---|---|---|
| D1 | trusty-search | An index identity and a per-file content hash (below). | Index state never reads Searchable, and a feed pass returns at best `Unconfirmed` (§4.9.2 rule 5). The probe uses chunk coverage only (§4.9). |
| D2 | trusty-search | r7: `search.index.config.get` adds `include_paths` and `path_filter` to the filters it already returns (`extensions`, `exclude_globs`, `extra_skip_dirs`, `invalid_exclude_globs`; `service/server/index_config.rs:51-76@c1eed98441`). The preferred alternative is a per-path admission query: given an index-relative path, the daemon answers whether its admission policy admits it. That spares trusty-okf from copying the daemon's glob semantics (`repo_config.rs:364-390@c1eed98441`). Either form satisfies D2. | `rebuild` sends `search.index.reindex` only on an exclusion proof from the current `config.get` reply. A binding that relies on an allowlist gets per-path pushes and removes only (§5.3.1 rule 4). |
| D3 | The caller that creates the index (trusty-agents for an OKG) | r7.1: the index binding admits every path except `_sources/**` and `_state/**`. It states this as the literal `exclude_globs` entries `_sources/**` and `_state/**`, with no allowlist (§5.3.1 rule 1). | The per-pass scope check refuses the feed with `IndexScope` on any leak (§5.3.1 rule 3). A pushed concept that the binding refuses fails as F2 and stays Pending. |

**D1 interface (proposal for trusty-search).** Three additions. Each is additive, so an older client ignores it.

1. **`search.index.status` gains `identity: { instance_id, generation }`.**
   - `instance_id` is an opaque string of at least 128 random bits. The daemon mints it when it creates an index ID. Deleting and re-creating the same index ID mints a new value. A value is never reused. It stays the same across daemon restarts, warm boots and ordinary writes.
   - `generation` is an unsigned 64-bit integer. It starts at 1 for each `instance_id`. It increases after any event that can leave a chunk absent without a per-path remove for that chunk: a corpus reset or clear, a restore from a snapshot or backup, a schema migration that discards or fails to carry content (a non-null `migration_error`), a corpus reopen that falls back to an empty corpus, and a full reindex that starts from an empty corpus. The new value is durable before the index answers its next query. A per-path put, a per-path remove and a prefix remove do not change it.
   - When the daemon cannot read its stored identity, it mints a new `instance_id`. It never reports a guessed old value.
2. **The `search.index.file.put` reply gains `content_sha256` and `identity`.** `content_sha256` is the lowercase hex SHA-256 of the exact `content` bytes that the daemon indexed. `identity` is the index identity at commit. A tombstone reply with `removed: true` carries both too.
3. **A new method, `search.index.file.status`.** Request: the index-scoped envelope with `body: { path }`. Reply: `{ path, present, content_sha256, chunk_count, identity }`. `present` is true when the index holds chunks for the path. `content_sha256` is the hash of the content those chunks came from, or null when `present` is false. trusty-search already keeps a per-path content hash, which a remove forgets (`crates/trusty-search/src/service/server/files.rs:132@c1eed98441`); this method exposes it.

**How trusty-okf uses D1.** The feed stores `identity` with every Acknowledged, Searchable and Withdrawn record (§4.9.2). It counts a put as acknowledged only when `content_sha256` equals the hash of the bytes it sent (F4 otherwise). The probe may use `search.index.file.status` as its revision evidence (§4.9).

## 6. Acceptance criteria {#SPEC-OKF-01-6}

### 6.1 Accepting this spec

Each item ends with its state at r7. "Satisfied" means the spec text meets the item. "Open" means an action outside this file remains.

1. §3 (a) is recorded as decided, with its date and the ADR-OKF-0001 pointer. **Satisfied (r2).**
2. The four 2026-11-03 owner inputs are recorded in §1.1–§1.4, each with its date, its approved text and its proposal list. **Satisfied (r3).**
3. r7, narrowed by owner-choices item 1 A: Bob has decided owner-choices items 1–18, which cover §3 (b)–(e), (g), (i)–(m), (q), (r), sub-question (c2), §7 and five choices without a §3 question. Items 19–26, which cover §3 (f), (h), (n), (o), (p) and three choices without a §3 question, are decided (recommended), revisitable at implementation. A change to one of items 19–26 does not reopen acceptance. **Satisfied (r7, §1.5).**
4. Bob has accepted or changed the proposals listed under §1.1–§1.4. **Satisfied (r7.1).** The 06:58Z ruling decided the 26 listed choices, not the proposal lists. Owner acceptance of the spec as written, 2026-11-03 09:16Z (Status line), settles this item (#10743).
5. Bob has settled the §3 (l) and §3 (m) conflicts with standing rulings, and any changed ruling is recorded with its date. **Satisfied (r7).** §3 (l) is decided C, which narrows SENT-only to the automated refresh, recorded in §1.5. The §3 (m) conflict with ADR-0024 is settled by item 3 A, which changes no ruling. The DOC-63 amendment is item 13.
6. The DOC-55 amendments in §2.16 are filed in trusty-tools, or Bob records that they are not needed. **Open (pre-implementation filing).** 3p A keeps DOC-55 §3.3 unchanged. The §8 item 3 amendment for the worker, with the OS sandbox condition of item 9 B, is still needed.
7. The sync rule of §2.12 and §4.11 is filed against ADR-0022 / DOC-56 Phase 2 in trusty-tools before implementation starts. The rule: ADR-0022 sync excludes `_sources/ops/`, and it never commits while the bundle lock is held. The filing gives the dependency an owner. (r5) **Open (pre-implementation filing).**
8. Each pick is written into this file with its date, and every "depends on 3x" passage is resolved to the chosen option. **Satisfied (r7).**
9. ADR-OKF-0001 is filed in trusty-tools `docs/adr/`. Where a later pick changes ADR-0041 again, a further amendment is named. r7: 3e A changes ADR-0041 Decision 2. The named amendment moves feed-loop ownership to trusty-okf's `feed` module, while trusty-search keeps chunking, embedding and ranking. It may be a revision of ADR-OKF-0001 or a separate ADR. 3g B needs no amendment. **Naming satisfied (r7). Filing open (pre-implementation filing).**
10. No section contradicts the 2026-08-01 / 2026-08-03 owner decisions, unless a decision records the change. **Satisfied (r7).** The one change, SENT-only narrowed to the automated refresh, is recorded in §1.5 and §3 (l). Trust labels before connector grants and the grant review for new source types stand (§2.15).
11. The external dependencies of §5.6 are filed before implementation starts (r6; r7 corrections). D1, the index identity and per-file content hash, is filed against trusty-search. D2 is filed against trusty-search as `include_paths` and `path_filter` in `search.index.config.get`, or as a per-path admission query. D3 is filed against trusty-agents (r7.1): the binding admits every path except `_sources/**` and `_state/**`, stated as those two literal `exclude_globs` entries. Each filing names the §5.6 interface and gives the dependency an owner. trusty-okf builds none of them. **Open (pre-implementation filing).**
12. The Status line reads "Accepted". **Satisfied (r7.1).** The Status line reads "Accepted r7.1", owner acceptance 2026-11-03 09:16Z (#10743).
13. (r7) DOC-63 S-3.3 is amended in trusty-tools so SENT-only binds the automated OKG refresh only, and an operator-initiated inbound import needs the capability-grant review first (§3 (l), item 2 C). **Satisfied ([#10742](https://github.com/bobmatnyc/trusty-tools/issues/10742), merged in [#10810](https://github.com/bobmatnyc/trusty-tools/pull/10810)).**
14. (r7) The capability-grant reviews that DOC-63 Q2 requires are scheduled for inbound Gmail (item 2 C) and native Google Doc export (item 10 B). They gate the first import against a real account, not implementation. DOC-63 places them after Phase A. **Open (gates first real-account use).**
15. (r7) Before the worker is built, a check confirms that a `[workspace] exclude` directory with its own `rust-toolchain.toml` keeps the 1.94 workspace build clean (§3 (n), item 20 B). ADR-0029 records the excluded worker directory as outside the workspace MSRV, or its owner records that no note is needed. **Open (pre-implementation filing).**
16. (r7) No ADR-0024 amendment is filed. Item 3 A keeps sub-agents to the in-process tier (§2.15). **Satisfied (r7).**

**Summary at r7.** Satisfied: items 1, 2, 3, 4, 5, 8, 10, 12, 13 and 16, and the naming half of item 9. Open before acceptance: none; owner acceptance settled items 4 and 12 on 2026-11-03 09:16Z, and item 13 merged in #10810 (#10743). Open before implementation: items 6, 7, 9 (filing), 11 and 15. Open before the first real-account import: item 14.

### 6.2 First implementation milestone

Scope: the `format`, `profile`, `bundle`, `build`, `state`, `import` and `extract` modules, the moves in §2.7.2, the feed (3e A), and the extraction worker. r7 resolves every "if 3x = …" condition to the decided option. Criterion 41 is withdrawn, and criteria 144–147 are new (r7). Criteria 60–64 restate the approved acceptance of §1.1. Criteria 65–105 test proposals and change if Bob changes the proposals. Criteria 106–124 (r4) and 125–127 (r5, r5.1) test the review-blocker fixes. Criteria 118–119 test a rule derived from §1.1; the rest test proposals. Criteria 128–143 (r6) test the acceptance-reconciliation fixes. Criteria 133, 134, 135 and 142 exercise the §1.1 cases of the reconciliation file: case 1 (save succeeds and index fails), case 2 (content changes) and case 3 (an acknowledgement is not retrieval proof). From r6, no state reads Searchable without dependency D1 (§4.9.2 rule 5). So every criterion that expects Searchable, for example 118 and exit code 0 with `--feed`, runs against a fake daemon with D1.

**Format and bundle**

1. Lenient `Bundle::open` returns `Ok` for each of the four bundles under `bundles/` in the Google spec repo at `ad30107c`. (The repo's `samples/` directory holds seed lists, not bundles.)
2. For every concept in those bundles, parse, render and parse again yields equal frontmatter, and every unknown key survives.
3. Strict write refuses a concept with a missing or empty `type`, returns `Conformance`, and changes no file.
4. A bare `verified` mapping parses as a one-element list.
5. `trust_tier` matches OKF §5.3 for no `verified`, machine-only `verified`, and any `human:` actor.
6. `staleness` returns `Unknown`, with a warning finding, for a date-only or offset-less `stale_after`.
7. A second `regenerate_indexes` run writes zero bytes.
8. `regenerate_indexes` leaves a hand-written `index.md` (no generator marker) byte-identical and reports an info finding.
9. After `move_concept`, no link in the bundle points at the old ID, and `log.md` has one new entry.
10. trusty-okf exposes no public function that deletes a concept file.

**Consolidation and the boundary rule (§2.7)**

11. `cargo tree -p trusty-okf -e normal,dev,build` lists no trusty-agents, trusty-mcp, trusty-mcp-services, trusty-gworkspace or connector crate (B1, B2).
12. A grep of `crates/trusty-okf/src` finds no `std::env::var`, `knowledge_dir`, `TRUSTY_AGENTS_HOME`, `KB_KNOWLEDGE_DIR`, `home_dir`, `.trusty-agents`, `assistant-okg-`, `search.index.create` or `search.index.delete` (B3–B5).
13. `Locator` in trusty-okf has no `Gmail` or `Drive` variant, and a `registry.toml` written by trusty-kb with `gmail` and `drive` rows loads without losing a row (B6, §2.9 item 10).
14. The crate has no MCP tool descriptors and no stdio server binary (B7).
15. The crate builds with `--no-default-features`, and then has no async runtime and no socket client (B8).
16. trusty-agents builds with trusty-okf in place of trusty-kb. Its existing OKG tests pass (`crates/trusty-agents/src/tools/okg/tests.rs`, `stores/okg_graph_tests.rs`), and the feed tests from `stores/index_feed_tests.rs` pass in trusty-okf (3e A).
17. No crate in the workspace depends on trusty-kb, and `crates/trusty-kb` is gone.

**v0.2 compatibility (§2.8)**

18. A trusty-kb concept with an offset-bearing `updated` migrates to `generated.at` with the same instant, and `updated` is removed.
19. A trusty-kb concept with a date-only `updated` keeps `updated`, gains no `generated`, and yields a warning finding.
20. A plain-string `sources` list becomes `sources[]` mappings with the same strings as `resource`, in the same order, with no `id`.
21. `source_id`, `source_kind`, `source_item_id`, `source_timestamp`, `trust`, `created`, `aliases`, `same_as`, `uid` and every wiki-link field are byte-identical after migration.
22. (3b B) A v0.1 concept with `timestamp` and `# Citations` reads as `generated.at` and `sources`, and its body is unchanged.

**Lossless migration (§2.9)**

23. On a fixture tree that has hand-owned, ledger-owned and tombstoned concepts, a migration keeps the concept ID set and every body byte for byte.
24. The audit fails, restores every changed file, and returns `MigrationLoss` when a test hook drops one `sources` entry during apply.
25. A second migration plans zero changes and writes zero bytes.
26. Every `log.md`, `_state/conversion-log.md`, ledger file and journal file is equal or only appended after a migration.
27. A re-ingest of a changed store item keeps `status`, `supersedes`, `superseded_by`, `stale_after` and the verification record.
28. `rebuild` leaves every hand-owned concept byte-identical, and refuses with `Conflict` when a ledger-owned concept was hand-edited.
29. Two rebuilds from equal store contents and an equal ledger, with no hand edits, run at different wall-clock times, yield byte-identical `.md` trees.
30. A `build` with no store change writes no files and yields an empty feed backlog.

**Verification invalidation (§2.10)**

31. A `put` that changes the body moves every `verified[]` event to the verification record with `invalidated_at` and `invalidated_by_hash`, removes `verified` from the frontmatter, sets `generated`, and makes `trust_tier` return `Unverified`.
32. A `put` that changes a non-lifecycle frontmatter key (for example `description`) does the same as criterion 31.
33. A `put` that changes only `status` or `stale_after` keeps `verified[]` and leaves `generated` unchanged.
34. A migration changes no concept's content hash.
35. `verify` with a stale content hash returns `Conflict` and writes nothing.
36. After an out-of-band body edit to a verified concept, the next read returns a warning finding, and `revision_state` returns Unverified. `trust_tier` reads only the raw frontmatter, so it returns the same tier as before the edit (criterion 64).

**Correction and tombstone lifecycle (§4.8)**

37. A rendered tombstone passes `trusty_common::knowledge_document::is_tombstone`.
38. trusty-okf refuses to write `source_status: deleted` on a concept without `source_id`.
39. A tombstoned concept whose store item reappears returns to Live, keeps its editorial keys, and gains an `Ingested` ledger record after the `Tombstoned` one.
40. (3c A) After `supersede`, the old file still exists with `status: deprecated` and `superseded_by`, and the new concept has `supersedes`.
41. Withdrawn in r7. It tested 3c B, which was not chosen.
42. After a feed pass, tombstoned and deprecated concepts return no hits (3i A), including after a watcher-triggered re-index of the file (F9).

**Index reconciliation (§5.4)** — run against a scripted fake daemon, plus one live-daemon test for F6.

43. F1: with no socket, the pass returns `NotFed`, `pending` equals the backlog size, and the journal file is byte-identical.
44. F2: with one of three puts failing, two journal lines are appended, and the pass returns `Partial` with one error.
45. F3: when the journal append fails after a confirmed put, the pass returns `JournalWrite`, counts no task as indexed, and the next pass re-pushes the task.
46. F4: a put reply with `indexed: false` appends no journal line and counts as a failure. A tombstone reply with `removed: true` counts as success.
47. F5: an index whose root does not contain the bundle root, or whose status carries no absolute root, makes the pass return `RootMismatch` before any `file.put`.
48. F6: after feeding a shorter revision, `search.chunks.list` for that path returns only the new chunks.
49. The journal line records the hash of the bytes sent. A test changes the file between backlog and push, and the next backlog lists the path again.
50. A hand-owned concept enters the backlog, and a moved concept's old path becomes a remove task.
51. The CLI `feed` verb exits non-zero on `Unconfirmed`, `Partial` and `NotFed` (3g B, 3e A).

**Path confinement (§2.11)**

52. `ConceptId::parse` refuses each of: `""`, `/a`, `a\b`, `a//b`, `./a`, `a/../b`, `..`, `C:`, `C:foo`, `a/C:b`, `a\u{0}b`, `index`, `x/Log`, `_sources/a`, `.git/config`, `a.md`.
53. A put whose path crosses an in-root symbolic link returns `PathEscape` and writes nothing.
54. `Bundle::open` on a missing root returns an error.
55. `move_concept` leaves a relative link that climbs above the root unchanged and reports it.
56. `supersede` with a `superseded_by` target that does not parse as a concept ID is refused.
57. No write path in trusty-okf calls an HTTP route. The crate has no HTTP client dependency.

**ADR boundaries (§2.12)**

58. ADR-OKF-0001 keeps ADR-0041 Decisions 2–5. Its text says which ADR-0041 sentences it replaces.
59. No trusty-okf module serves an agent-consumed MCP tool (ADR-0040, ADR-0041 Decision 4).

**State separation (§1.1, approved; §4.9)**

60. Save succeeds and index fails: a `put` lands, and the feed pass fails (F1). `revision_state` returns Saved and Pending. The feed returns `NotFed`. No report, status line or exit code says "synced", "searchable" or "done".
61. Content changes: a verified concept gets a content-changing `put`. The old events remain in the verification record as history, each with `invalidated_at` and `invalidated_by_hash`. `revision_state` returns Unverified with `history ≥ 1`.
62. The current revision stays Unverified until evidence matches it. A `verify` with the old hash returns `Conflict`. A `verify` with the new hash returns Verified.
63. An index acknowledgement alone is not retrieval proof. With a fake daemon that confirms `file.put` but returns no hit for the probe, the index state is Acknowledged, not Searchable. The pass returns `Unconfirmed`, and the bundle is not Synced (F12).
64. The OKF trust tier and the verification state are reported separately. After an out-of-band body edit to a human-verified concept, `trust_tier` on the raw frontmatter returns `HumanReviewed`, while `revision_state` returns SavedExternal and Unverified with a warning finding.
65. A lifecycle-only `put` (`status` or `stale_after`) keeps the verification state and moves the index state to Pending.
66. A crash injected between the revision-journal line and the rename leaves the concept's previous revision as Saved. The unwritten bytes never read as Saved.
67. `migrate` leaves every concept's verification state unchanged and moves changed files to Pending.
68. After an explicit `reindex` that leaves the index identity unchanged, every Searchable path reads as Acknowledged until the next probe passes. r6: after a reindex that changes the identity, every such path reads Pending (§4.9.2).
69. `retrieve` returns a hit whose text is absent from the current file, marks it `stale_revision` and reports it Unverified. r7 (item 22 B): a hit for a tombstoned concept, or for a concept that reads WithdrawPending or Withdrawn, is dropped.
70. Each row of the §4.9 operation table has one test that checks all three axes after the operation.

**Document-tree import (§1.2 scope; §2.14, §4.10 proposals)**

71. After `import run` on a fixture tree, the SHA-256 of every file and directory listing under the source root is unchanged.
72. `import plan` writes zero bytes to the bundle and the source, and lists every item with its ID, action and extractor.
73. The bundle's ID tree mirrors the source hierarchy. `a.md` maps to `a`, and `a.pdf` maps to `a.pdf`.
74. Source names refused by §2.11 (`index.md`, `C:x.md`, `_x/y.md`) map to normalized IDs with the `~<h8>` suffix, and the same input maps to the same ID on two machines.
75. Two source paths that collide are both refused with `collision`, and neither is written. A ledger-pinned ID keeps its concept when a new colliding file appears.
76. Every imported concept carries `sources[]` with the source-relative path, plus `source_id`, `source_kind`, `source_item_id` and `source_timestamp`. No absolute path appears in the bundle.
77. A second `import run` with no source change writes zero bytes. A changed source file updates its concept, keeps the editorial keys and moves verification to history.
78. A hand-edited imported concept is refused with `Conflict` on re-import and reported.
79. A removed source file is tombstoned only when the run is `deletion_safe` (r6, §2.13). Otherwise it is reported as missing.
80. Every skipped item in the report carries one reason from §2.14 rule 8. The report's item count equals the listed item count.
81. A symbolic link under the source root is reported as `symlink` and not read, including a link that points inside the root.
82. `import run` refuses with `SourceOverlap` when the bundle root is inside the source root, or the reverse.
83. A run killed after half its chunks, then re-run, yields the same bundle as one uninterrupted run. `import status` lists the killed run as interrupted.
84. No imported concept has `verified`. A source field named `approved` stays an extension key. r4: a source field named `verified` is quarantined under `source_fields` with a `quarantined_key` finding (§2.13.1).
85. With the feed failing, the import report gives `written > 0`, `searchable = 0`, `pending > 0`, says "not fully searchable", and the CLI exits 4.

**Email and Google Docs import (§1.3 scope; §2.15 proposals; fake adapters, no live account)**

86. An import with no account or source selection is refused with `SelectionRequired` and writes nothing.
87. A fake Gmail adapter with three pages imports every message. A run stopped at a ceiling reports `pagination_exhausted: false` and the cursor, and tombstones nothing.
88. Concept IDs derive from message, thread and file IDs. Two same-day messages with one subject get two concepts.
89. Each email concept keeps `thread_id`, `internalDate` and the `Date` header. Messages of one thread share a directory, and the thread concept links them in date order.
90. A second run over an unchanged mailbox writes zero bytes. A widened window fetches only the new messages.
91. A fake Docs export in Markdown or HTML keeps headings, lists, tables and links in the concept body. A base64 export with no extractor is reported `unsupported_format`.
92. A message with no decodable text part is reported `degraded: snippet_only`. Under the default policy (3k A) no concept is written for it. Under a per-store policy that allows snippets, its snippet is written and flagged.
93. The default policy (3k A) imports no raw MIME and extracts no attachment. Each attachment is listed by name, type and size. Quoted text stays in the body, marked as quoted.
94. A Drive item marked `trashed` on a complete recursive listing is tombstoned and reported.
95. A run interrupted mid-chunk resumes from the ledger and re-fetches at most one chunk.
96. trusty-okf's import code receives no credential: the `ImportSource` API has no token, secret or account-secret parameter (B6, B10).
97. (3m A) The CLI imports through a fake adapter process end to end, and the adapter is reaped on cancel.
98. (3l C) In a run that is not operator-initiated, including every scheduled refresh, the `gmail` adapter's listing query always carries `in:sent`, and a store or caller query cannot remove it (DOC-63 S-3.3 as amended). An operator-initiated run with an inbound query is refused until the store holds a passed capability-grant review. After the review, the run imports the inbound messages with the trust label `UntrustedExternal`. This test lives with the adapter in trusty-agents.

**Extraction worker (§1.4 design; §2.16 proposals)**

99. `cargo tree -p trusty-okf --all-features` lists no `xberg`, `ort` or `candle-core` (B9).
100. The handshake agrees on a protocol version. A test-double worker with no common version is killed, and its items are reported `worker_incompatible`. Other items still import.
101. With no worker at the configured path, `import plan` shows the affected items, `import run` reports them `worker_absent`, and the CLI exits 3.
102. Limits hold: an oversized input is not sent; an oversized output, a hang past 30 s and a frame over the cap each fail only their item.
103. Cancel, timeout and host panic each leave no process in the worker's process group, and every child is reaped. A test-double child that ignores SIGTERM is killed by SIGKILL.
104. The worker's environment holds only the allow-listed variables, and its working directory is empty (B10). The worker binary has no network or model-download feature in `cargo tree -e features`.
105. Each fixture in Appendix B gives its expected outcome. The text PDF extracts twice to byte-identical output. Every extracted concept records `extracted_by`, `extract_protocol`, `xberg_version`, `extract_config` and the input SHA-256, and a configuration change re-extracts exactly those items.

**Multi-file crash recovery (r4, §4.11)** — fault injection through a test hook that ends the process at a named step boundary.

106. For each of `move_concept`, `supersede` (3c A), a content-changing `put` (3d C), and `migrate`, the hook crashes the process at every step boundary in turn: after staging, after the intent record, after each file step and each append step, and before the completion record. After each crash, `Bundle::open` returns `Ok`. A roll-forward kind leaves a tree byte-identical to an uninterrupted run. A `migrate` leaves a tree byte-identical to the tree before the migration. Each byte-identical comparison covers every file outside `_sources/ops/` and `_sources/migrations/`.
107. After every crash point in criterion 106, the multiset of verification events, current and history, equals the multiset of an uninterrupted run (or of the pre-operation tree, for `migrate`). No link in the bundle points at a missing concept.
108. A crash during recovery itself, followed by a second `Bundle::open`, gives the same end state as one uninterrupted recovery. A torn intent line leaves the bundle byte-identical to the pre-operation state, and the next open removes the staging directory. Each byte-identical comparison covers every file outside `_sources/ops/` and `_sources/migrations/`.
109. A file changed out of band between the crash and the next open is not overwritten. `Bundle::open` reports `recovery_conflict`, a write to that operation's paths returns `RecoveryPending`, and `Bundle::recover` with either direction keeps a copy of the changed file under `conflict/`.

**Rebuild history and hand edits (r4, §4.5)**

110. A bundle whose `log.md` holds deprecate, supersede, move, verify and correction entries is rebuilt. Removing the lines the report lists as added yields the old `log.md` byte for byte.
111. A ledger-owned concept whose ledger record has no `written_bytes` stays byte-identical after `rebuild` and is reported `Conflict { reason: "no_written_hash" }`. A concept whose disk bytes differ from its stored hash is reported `hand_edit` and stays byte-identical. The other concepts in the same run rebuild.
112. The hook crashes `rebuild` before its intent record and at each swap step. A crash before the intent leaves the tree byte-identical. A crash after it rolls forward on the next open, and no concept file holds bytes other than its pre-image or its post-image. A hand edit made to a concept between generation and swap stays byte-identical and is reported as `recovery_conflict`. A line added to `log.md` between generation and swap stays in `log.md`: the swap stops before the `log.md` rename, `log.md` stays byte-identical to its edited state, and the `recovery_conflict` finding names `log.md`.

**Raw-preserving round trip (r4, §2.2.1)** — fixtures: date-only `stale_after`, offset-less `stale_after`, `generated: {by: x}` with no `at`, `status: archived`, `type: [a, b]`, `verified: "yes"`, an unknown key in flow style with a YAML comment, and an unknown nested mapping.

113. For each fixture, under `WriteMode::Lenient`, `deprecate` and a `put` that changes only `description` leave the body and every top-level frontmatter entry the operation does not own byte-identical (§2.2.1). `deprecate` owns `status`. The `put` owns `description` and the §2.10 keys, including `generated`, `verified`, `legacy_verified`, `legacy_generated` and the verification-record keys. Only owned entries differ.
114. For each fixture, a read followed by a `put` of the unchanged concept writes zero bytes. Each malformed value reads as `Field::Malformed` with a warning finding, and `generated: {by: x}` reads as valid with `at` absent.
115. A bundle with `notes/a:b.md`, `Index.md` and `_drafts/x.md` opens with `Ok` and one info finding per file. After `regenerate_indexes`, a `move_concept` of a concept they link to, and a `rebuild`, the three files are byte-identical. A write that targets one of them returns `InvalidId`.

**Protected keys at the ingest boundary (r4, §2.13.1)**

116. A fake source adapter, a fake worker's `fields` and a source `.md` file's frontmatter each supply every key in the protected set, including `verified`, `status`, `supersedes`, `superseded_by`, `generated`, `stale_after`, `source_status`, `tombstoned`, `trust`, `source_timestamp` and a verification-record key. The written concept carries none of these source values at the top level. Each value sits unchanged under `source_fields`, and the report lists one `quarantined_key` finding per key and item. `is_tombstone` returns false, and the trust label equals the store registration.
117. A source item supplies a verification-record hash equal to the deterministic content hash of the imported revision. After import and after a re-import, `revision_state` returns Unverified with no history.

**Machine-local index state (r4, §4.9.1; derived from §1.1)**

118. Machine A feeds a bundle until every concept is Searchable in its index state directory. The bundle tree, `_sources/` included, is copied to machine B, which has an empty index state directory and a fake daemon. On B, `revision_state` returns Pending for every concept, and `state_summary` reports zero Acknowledged and zero Searchable.
119. After a feed pass, a recursive listing of the bundle root shows no file that the pass created. A legacy `_sources/<id>.index.jsonl` stays byte-identical and is never read as Acknowledged. An `IndexStateDir` inside the bundle root is refused with `StateDirInBundle`.

**Migration restore (r4, §2.9)**

120. A test hook changes one body byte of a concept during migration apply. The audit fails. Every changed file is restored, each restored file's SHA-256 equals the manifest's old hash, and `MigrationLoss` names the body item. With one backup blob corrupted, the result also lists `restore_failed` for that file and the blob stays in place.

**Worker deadlines (r4, §2.16)** — each test runs under a test-level timer equal to the sum of the deadlines plus 5 s, and fails if the timer fires.

121. A test double that never answers `hello` is killed and reaped within the handshake deadline plus the kill sequence. Its items fail with `handshake_timeout`, the report records `WorkerError::HandshakeTimeout`, other items import, and the CLI exits 3.
122. A test double that never reads stdin, so the host's `hello` write blocks, gives the result of criterion 121. A test double that sends a non-`hello` first frame gives `protocol_error`.
123. A test double that ignores `shutdown`, a closed stdin and SIGTERM is killed by SIGKILL within the shutdown deadline plus 2 s. The child is reaped, and the report records `WorkerError::ShutdownTimeout`. Committed items keep their outcomes.
124. With the reap step forced to exceed its deadline through a test hook, `import` returns within the reap deadline after SIGKILL and records `WorkerError::ReapTimeout`.

**Delta-review fixes (r5)**

125. A concept holds `verified: "yes"` and `generated: bot`. A `put` that changes only `description` writes `legacy_verified` and `legacy_generated`, each with the old value's raw bytes unchanged. The file then holds a new `generated` and no `verified`. The report lists one `legacy_value_moved` warning per key. A `verify` with the current hash on a concept with `verified: "yes"` moves the value the same way, and the content hash is unchanged. When `legacy_verified` already exists, the write returns `Conflict` and the file stays byte-identical.
126. For `move_concept`, a test hook edits one linking concept after the intent record and before that concept's file step. The operation stops before the step, writes no completion record, and reports `recovery_conflict` naming the path. The edited file stays byte-identical. No link in the bundle points at a missing concept. A write to the operation's paths returns `RecoveryPending`, and `Bundle::recover` with either direction keeps a copy of the edited file under `conflict/`.
127. A `migrate` crashes after two file steps, and a roll back restores both files from the manifest without a pre-image check, so the bundle matches its pre-migration bytes. After the conflict in criterion 126, `Bundle::recover` with either direction finishes, writes a completion record and clears the conflict marker. A following write to the operation's paths succeeds instead of returning `RecoveryPending`.

**Acceptance-reconciliation fixes (r6)** — each criterion names its reconciliation row. Criteria 128 and 141 use a live trusty-search daemon. The others use a scripted fake daemon. A fake daemon "with D1" serves the §5.6 D1 fields.

128. Row 1. A bundle holds `_sources/registry.toml`, `_state/conversion-log.md`, a staged `new/` file, a staged `old/` file and an `_sources/ops/<op-id>/conflict/` copy that contains a unique phrase. The index binding excludes `_sources/**` and `_state/**`. After a full `search.index.reindex`, and again after a watcher event on each of those files, `search.chunks.list` with the `_sources/` prefix and with the `_state/` prefix each return no chunk. A `search.query` for the unique phrase returns no hit. With the binding exclusion removed, the staged and conflict copies still return no chunk.
129. Row 1. A fake daemon returns one chunk for the `_sources/` prefix. The feed pass sends `search.index.files.remove` for that prefix, sends no `search.index.file.put`, appends no journal line and returns `IndexScope` naming the path. r7: against a fake daemon whose `search.index.config.get` reply shows an allowlist binding with no `include_paths` field and no exclusion of either tree, `rebuild` completes, and the fake daemon records zero `search.index.reindex` calls. With `exclude_globs` holding `_sources/**` and `_state/**` and an empty `invalid_exclude_globs`, `rebuild` sends one `search.index.reindex`. In criteria 129–135, the fake daemon receives no `file.put` for a path under `_sources/` or `_state/`.
130. Row 2. An ingested concept gets a `verify` with the current hash, then a `stale_after` renewal. Its source item then changes. `rebuild` updates the body, keeps the renewed `stale_after`, and moves the verification event to history: `revision_state` returns Unverified with `history ≥ 1`. The run reports no `hand_edit`, `caller_edit` or `no_written_hash` for the concept. A re-import of the same change through a docstore store gives the same result.
131. Row 2. An ingested concept gets `verify`, then `deprecate`, then a `stale_after` renewal, and its source item changes. Under (c2)(i), `rebuild` and a re-import each update the body, keep `status: deprecated` and the editorial keys, move the verification event to history, and report no `hand_edit` and no `Conflict { reason: "deprecated" }`. A content-changing `put` on another ledger-owned concept makes the next `rebuild` report `caller_edit` and leave it byte-identical. A `migrate` of a concept whose record has no `written_bytes` leaves the next `rebuild` reporting `no_written_hash` for it.
132. Row 3. A fake Drive adapter returns exactly 5,000 items, `complete: true` and a remaining page token. Nothing is tombstoned, and the report says `pagination_exhausted: false` and `deletion_safe: false`. With no remaining token but a run ceiling of 5,000, the report still says `pagination_exhausted: false`. A fake Gmail adapter that exhausts three pages reports `pagination_exhausted: true` and `deletion_safe: false`, tombstones nothing, and reports each absent item as `missing`. As a control, a recursive Drive listing below the ceiling with `tombstone_deleted` set reports `deletion_safe: true` and tombstones its one vanished item.
133. Row 4, case 3. A Searchable concept is edited only in its last paragraph. A fake daemon with D1 acknowledges the put, echoes the new `content_sha256`, still serves the old chunks to `search.query` and `search.chunks.list`, and reports the old `content_sha256` from `search.index.file.status`. The probe fails, the index state is Acknowledged, the pass returns `Unconfirmed`, and the bundle is not Synced. As a control, the same fake serving the new chunks and the new hash makes the concept Searchable.
134. Row 5, cases 1 and 3. Machine A feeds a bundle until every concept is Searchable against a fake daemon with D1. The fake daemon then deletes and re-creates the same index ID, which mints a new `instance_id` and an empty corpus. With the new live identity, `revision_state` returns Pending for every concept, and `state_summary` reports zero Acknowledged and zero Searchable. The bundle is not Synced, and no report, status line or exit code says "synced" or "searchable". The next feed pass pushes every concept again. A generation increase with the same `instance_id` gives the same result. As a control, an unchanged identity keeps every concept Searchable.
135. Row 5, case 1. After criterion 134's first feed, the caller passes `LiveIdentity::Unreadable`, and `revision_state` returns Pending for every concept. With the old identity passed again, each reads Searchable. Against a fake daemon without D1, every put is acknowledged and every probe passes by chunk coverage, yet the pass returns `Unconfirmed`, `state_summary` reports zero Searchable, and `import run --feed` exits 4. A second pass over the unchanged bundle sends no `file.put`. A fake daemon with D1 whose put reply carries a `content_sha256` that differs from the sent bytes makes that task fail, and no journal line is appended.
136. Row 6. A bundle holds `notes/sub/a` and `notes/a` from one store. The source of `notes/sub/a` vanishes on a deletion-safe listing. Only `notes/sub/a` is tombstoned, and `notes/a` stays byte-identical. When the source reappears, only `notes/sub/a` returns to Live.
137. Row 7. A 50,000-character source file imports as one concept and one ledger item, with no `collision`. The concept body holds the whole extracted text. A repeat import writes zero bytes. After the file vanishes on a deletion-safe listing, its one concept is tombstoned, and no other concept changes.
138. Row 8. A docstore holds `a.md`, `a.txt`, `nested/c.md` and `nested-c.md`. `build` reports all four items as failed with `collision`, writes none of their concept files, and counts none as written. `rebuild` gives the same result. After `a.md` alone is built, adding `a.txt` and running `build` refuses `a.txt` with `collision`, and concept `a` stays byte-identical. `import` of the same four files maps them to four distinct IDs under §2.14 rule 3 and reports no collision.
139. Row 11. A fake adapter process prints its environment. A spawn through the adapter API (3m A) with a caller-supplied list yields exactly that list, with no variable whose name or value trusty-okf added. The extraction worker's environment holds only `LANG` and `TZ`.
140. Row 13. With `Notes/A` present, `put("notes/a")` returns `Conflict { reason: "fold_collision" }` and changes no file. With an NFC ID present, a `put` of its NFD spelling returns the same. A `move_concept` to `notes/a` returns the same. The test runs on a case-sensitive and on a case-insensitive file system, with equal results.
141. Row 14. Under 3d C, a concept accumulates 2,000 invalidated verification events. Its frontmatter holds at most 10 history events, and `revision_state` reports `history` = 2,000. The concept is then tombstoned. `is_tombstone` returns true on the file, and a watcher event leaves `search.chunks.list` for the path empty. A second concept carries an out-of-band 70 KiB unknown key. Its tombstone lands with a `tombstone_undetectable` error finding, and its index state reads WithdrawPending, never Withdrawn.
142. Row 17, case 2. A legacy bundle holds concepts with human `verified` events that carry no hash. After `Bundle::open`, `migrate`, `adopt` (3q C; r7.1: a `Human` actor, `confirmed: true` and each concept's current byte hash) and `rebuild` with its equality proof (3q C), in turn, `revision_state` returns Unverified with `history ≥ 1` after each step. No event in frontmatter or in the verification record has gained a hash. A `verify` with the current hash then returns Verified. r7.1: on a second legacy bundle, `adopt` with a `Process` actor returns `AdoptRefused { reason: "not_human" }`, and with `confirmed: false` returns `AdoptRefused { reason: "not_confirmed" }`. A concept edited after its byte hash was taken is listed in `refused` with `byte_hash_mismatch`, and the next `rebuild` leaves it byte-identical with `Conflict { reason: "no_written_hash" }`. In each refused case no file or ledger record changes.
143. Row 17. On a legacy concept with hashless events, a content-changing `put` moves every hashless event from frontmatter into the verification record with reason `legacy_unbound`. On a second legacy concept, `deprecate` and a `stale_after` renewal leave the hashless events byte-identical in frontmatter.

**Owner decisions without a §3 question (r7)** — fake worker, fake adapters and a scripted fake daemon.

144. Item 3 A. An import with `child_processes: false` over a local tree holding `a.md`, `b.txt` and `c.pdf` imports `a` and `b.txt` and skips `c.pdf` with `policy_refused`. A process-spawn hook records no child process. The same setting with a `gmail` store is refused with `policy_refused` before any write. r7.1: the library's error-to-exit-code mapping (§4.10) maps that refusal to exit code 2. No CLI flag sets `child_processes`, so the test does not run the CLI.
145. Item 9 B (r7.1). The host decides from its own spawn evidence, never from the worker's report. When that evidence shows no applied sandbox and the worker's `hello` reports `sandbox: none`, items from a `gmail`, `drive` or `gdocs` store are skipped with `sandbox_required`, and local-tree items still extract. A test-double worker that reports a sandbox the spawn evidence does not show gives `worker_incompatible` for the run, and the host sends it no item. A worker spawned under the sandbox that reports `sandbox: none` also gives `worker_incompatible`. When the spawn evidence shows the sandbox, a test-double worker that tries to open a socket and to read a file outside its working directory fails both attempts.
146. Item 19 B. A concept with a current human `verified` event gets a `put` from a `process:` actor that moves `stale_after` later. The write returns `Conflict { reason: "stale_after_extension" }` and the file stays byte-identical. The same actor moving `stale_after` earlier succeeds. A `human:` actor with explicit confirmation may move it later. r7.1: a `verify` of the current revision whose `Verification.stale_after` holds a later value moves it in the same write, and the projected `verified[]` event carries no `stale_after`. The same `verify` with a stale `seen` hash returns `Conflict` and leaves the file byte-identical.
147. Item 21 B. A concept extracted by worker version 1 is verified. Worker version 2 re-extracts it to the same text with a new `extracted_by`, `xberg_version` and `extract_config`. The content hash is unchanged, and `revision_state` stays Verified. A re-extraction that changes the text, or a source change to a recipient field, makes it Unverified with `history ≥ 1`.

## 7. Where the crate lives {#SPEC-OKF-01-7}

**Decided 2026-11-03 (owner): A.** Owner-choices item 18. The §3 (a) ruling did not decide this. ADR-OKF-0001 holds under every option below.

**Question.** Where does the `trusty-okf` crate live?
- A. A crate in the trusty-tools workspace (`crates/trusty-okf`). This project directory keeps the spec and session notes. **Chosen.**
- B. Its own repo, depending on trusty-common from crates.io. Not chosen.
- C. Its own repo now, moved into the workspace once the format settles. Not chosen.

**Trade-offs (survey §5):**

- **Dependencies.** In the workspace, trusty-okf uses `path` deps on trusty-common. Its own repo needs a crates.io pin on trusty-common 0.67.x for `search_rpc` and `knowledge_document`.
- **Consumers.** trusty-agents depends on this code today. With B, trusty-agents must depend on a published trusty-okf or a git dep. That conflicts with §3 (h) = A.
- **Publishing.** trusty-kb is deliberately `publish = false`, to avoid pinning the schema (`crates/trusty-kb/Cargo.toml:17-36@c1eed98441`). B makes publishing close to mandatory.
- **Versioning.** The workspace versions each crate independently ([#343](https://github.com/bobmatnyc/trusty-tools/issues/343)). A workspace crate keeps its own version, so A does not tie trusty-okf to other crates' releases.
- **Release flow.** Version bumps, tags and `cargo publish` run through local-ops and the cargo-publish skill (`docs/reference/release-workflow.md`). A workspace crate uses that flow as is. B needs its own.
- **Cross-crate changes.** A feed change can touch trusty-okf, trusty-common and trusty-search together. A lands that in one PR. B needs a publish-then-bump cycle.
- **Consolidation.** The moves in §2.7.2 use `git mv` inside one repo under A, which keeps file history. Under B or C, history crosses a repo boundary (inference).
- **Session.** This session runs in a separate project directory. With A, code work happens in a trusty-tools worktree, and this directory holds only the spec. With B, this directory becomes the repo.
- **Workspace mechanics.** Adding a directory under `crates/` joins the workspace automatically. `default-members` needs a manual entry.

**Reason for A.** Bob's requirements fix a separate crate and a separate session, not a separate repo (inference). A keeps path deps, `publish = false` (§3 (h) A) and one-PR cross-crate changes, while every consumer lives in the workspace. Code work happens in a trusty-tools worktree.

## Out of scope

- Wider document stores and repos. A later investigation only (§1 item 5). Import of a document tree, an email source or Google Docs into a bundle is in scope (§1.2, §1.3). A general document-store or search service is not.
- Connector code. Email and Google Docs import move into scope through the common import interface (§2.13, §2.15). Their authenticated adapters stay outside trusty-okf. Slack, Notion and Granola ([#4546](https://github.com/bobmatnyc/trusty-tools/issues/4546)–[#4549](https://github.com/bobmatnyc/trusty-tools/issues/4549)) stay out of scope. They remain gated on Phase A and on capability-grant review (§1.3).
- Account access, credential grants and bulk import of any real account. §1.3 authorizes design only.
- OCR and ML-model extraction (3p A; revisitable at implementation).
- A plugin framework for trusty-search extractors (Xberg assessment `:89-160`).
- The trusty-mcp-services OKG service (ADR-0041 Decision 4) and the `kb_put_entity` question.
- Any GUI.
- The scheduled refresh runner ([#4538](https://github.com/bobmatnyc/trusty-tools/issues/4538)). trusty-okf provides the idempotent `build` that the runner calls.
- Executing OKF Attested Computations. OKF defers the runtime protocol (§12).
- Any bridge to trusty-memory's KG.
- Changes to trusty-search ranking or chunking.

## Sources

External (pinned):
- OKF spec v0.2: [SPEC.md](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/SPEC.md) — §3 (L109), §3.1 (L134), §4.1 (L161), §5.1–5.5 (L287–L424), §6.1 (L439), §8 (L505), §9 (L533), §11 (L736), §12 (L766), §13 (L796).
- Google sample correction: [bundles/acme_retail/log.md](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/bundles/acme_retail/log.md).
- Google reference agent: [src/reference_agent/bundle/document.py](https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/ad30107c31c06aec8a7d5636e0d1058118604e6f/src/reference_agent/bundle/document.py).
- wmahfoudh/okf: [README.md](https://github.com/wmahfoudh/okf/blob/7d6286ba112177394f08bf7b79e56f54a1fd22ec/README.md), [DESIGN.md](https://github.com/wmahfoudh/okf/blob/7d6286ba112177394f08bf7b79e56f54a1fd22ec/DESIGN.md), [skills-claude/okf-validate/CHECKS.md](https://github.com/wmahfoudh/okf/blob/7d6286ba112177394f08bf7b79e56f54a1fd22ec/skills-claude/okf-validate/CHECKS.md). The repo has no root licence file; treat its licence as unspecified (inference, external report §4).
- Reddit thread https://www.reddit.com/r/LLMDevs/s/JtUce3LVGZ: it could not be fetched. This spec makes no claim about it.

trusty-tools (`@c1eed98441`, read-only):
- `docs/adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md`
- `docs/adr/0041-trusty-okg-native-crate-search-absorbs-okf-indexing.md`
- `docs/adr/0022-knowledge-tree-sync-model.md`
- `docs/adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md`, `docs/adr/0026-credential-grants-do-not-survive-delegation.md`, `docs/adr/0029-msrv-1-94-and-edition-policy.md`
- `docs/specs/okg-universal-importer.md` (DOC-55)
- `docs/specs/DOC-63-okg-sources.md`, `docs/specs/DOC-45-credential-authority-model.md`
- `crates/trusty-agents/src/tools/okg/gmail.rs`, `drive.rs`, `gapi.rs`
- `crates/trusty-kb/` (`Cargo.toml`, `src/schema.rs`, `src/entity.rs`, `src/store.rs`, `src/put.rs`, `src/structure.rs`, `src/convert.rs`, `src/roots.rs`, `src/okg/registry.rs`, `src/okg/ledger.rs`, `src/okg/ingest.rs`, `src/okg/docstore.rs`, `src/okg/policy.rs`, `src/okg/trust.rs`, `src/okg/index_journal.rs`)
- `crates/trusty-agents/` (`Cargo.toml:111`, `src/stores/index_feed.rs`, `src/stores/index_feed_rpc.rs`, `src/stores/okg_graph.rs`, `src/tools/memory/okg_fence.rs`)
- `crates/trusty-common/src/knowledge_document.rs`, `crates/trusty-common/src/search_rpc.rs`
- `crates/trusty-search/src/core/indexer/ingest/mod.rs`, `crates/trusty-search/src/service/rpc/writes.rs`, `crates/trusty-search/src/service/server/files.rs`
- r6: `crates/trusty-search/src/service/walker.rs`, `crates/trusty-search/src/service/index_admission.rs`, `crates/trusty-search/src/service/server/status.rs`, `crates/trusty-agents/src/api/server/knowledge_pipeline/indexing.rs`

trusty-tools worktree, not at `c1eed98441`:
- Xberg assessment (ar-T05): `docs/briefs/2026-11-03-xberg-assessment.md` in `.worktrees/f93609dd-53b5-4b85-ae0b-d8b0cd20ae7b`. It states it is based on `main@bcbea788` (`:580`). Its line numbers are cited as `xberg-assessment.md:<line>`.

Research notes (local, not committed): `/tmp/okf-research/external-okf-report.md`, `/tmp/okf-research/trusty-tools-okf-survey.md`; brief `architect/local/briefs/2026-11-03-trusty-okf-spec-brief.md` (sections from "Bob MUST-HAVE (via Codex 03:11Z)" to the end).

## Appendix A. Import report (proposal) {#SPEC-OKF-01-A}

Every import returns one report. The CLI prints it, and `--json` emits it. Each item appears exactly once in each stage.

| Stage | Field | Meaning |
|---|---|---|
| List | `listed` | Items the source adapter returned. |
| List | `pagination_exhausted` | r6: the engine's own finding that no page token or cursor remained and the listed count stayed below the ceiling (§2.13). The adapter's `complete` flag alone never sets it. |
| List | `deletion_safe` | r6: `pagination_exhausted`, a store kind that supports deletion detection, and `tombstone_deleted` set (C4). Always false for Gmail. Tombstones happen only when true. |
| List | `cursor` | Where a resumed run starts, when the listing was not complete. |
| Read | `read` | Items fetched and passed to an extractor. |
| Read | `skipped` | Items not read, each with one reason from §2.14 rule 8. |
| Read | `failed` | Items whose fetch or extraction failed, each with an error code (Appendix B for worker codes). |
| Read | `degraded` | Items read with a recorded loss, for example `snippet_only` (§2.15 item 11). |
| Write | `created`, `updated`, `unchanged`, `tombstoned` | Write outcomes. Their sum is `written + unchanged`. |
| Write | `write_failed` | Items read but not saved: `Conflict`, `collision`, conformance refusal or I/O. |
| Write | `missing` | Items absent from a listing that was not complete. Never tombstoned. |
| Index | `pending`, `acknowledged`, `searchable`, `withdrawn` | Index states of every touched concept after the run (§4.9). All are `pending` when no feed pass ran. |
| Verify | `verified_current` | Touched concepts whose current revision is Verified. A first import gives 0. |
| Verify | `verification_invalidated` | Touched concepts whose old events moved to history in this run. |
| Summary | `fully_searchable` | True only when every touched live concept is Searchable and every withdrawn one is Withdrawn. |
| Summary | `complete` | True only when `skipped`, `failed`, `write_failed` and `degraded` are all zero and `pagination_exhausted` is true. |
| Summary | `exit_code` | The §4.10 code. |

## Appendix B. Extraction worker protocol (proposal) {#SPEC-OKF-01-B}

**Transport.** The worker reads requests on stdin and writes replies on stdout. Each frame is a 4-byte big-endian length, a JSON header, and for `extract` a raw byte body. Raw bytes avoid base64 growth. stderr is diagnostic only.

**Messages.**

| Direction | Message | Fields |
|---|---|---|
| host → worker | `hello` | `protocol_min`, `protocol_max`, `host_version` |
| worker → host | `hello` | `protocol`, `worker_version`, `xberg_version`, `config_hash`, `formats[]`, `deterministic`, `features[]`, `sandbox` (r7: the active OS sandbox, or `none`; r7.1: a cross-check of the host's spawn evidence only, §2.16) |
| host → worker | `extract` | `request_id`, `type_hint {mime, extension, magic}`, `byte_len`, `sha256`, `limits {output_bytes, fields_bytes, notes}`, then the bytes |
| worker → host | `ok` | `request_id`, `input_sha256`, `text`, `fields`, `notes[]`, `provenance {pages, …}` |
| worker → host | `error` | `request_id`, `code`, `message` |
| host → worker | `shutdown` | — |

**Acquire role (r7.1).** A source adapter speaks the same transport and handshake in the `acquire` role (§2.15). Each host → adapter `acquire` request carries the store selection and `operator_initiated`, the `ImportSpec` value for the run. When the field is absent, the adapter treats the run as not operator-initiated: the `gmail` adapter keeps `in:sent` in the listing query (SENT-only, §2.15 "Inbound email"). The adapter, not the host, enforces SENT-only and the capability-grant check.

Protocol version 1 is the first version. A breaking change raises the version. The host supports a range, so one host release can talk to the previous worker release.

**Error codes.** Each is per item unless marked.

| Code | Raised by | Meaning |
|---|---|---|
| `unsupported_format` | worker | No xberg handler for the type. |
| `no_text_layer` | worker | A PDF with no text layer, with OCR off (3p A). Reported as a note with empty text, not as a failure. |
| `extract_error` | worker | xberg returned an error. The message is bounded and holds no content excerpt (DOC-55 E4). |
| `too_large` | host | Input over the limit; not sent. |
| `output_too_large` | host | Reply over a limit; worker killed. |
| `timeout` | host | Wall time exceeded; worker killed. |
| `crashed` | host | Worker exited or was signalled mid-request. Exit status recorded. |
| `protocol_error` | host | Bad frame, bad JSON, wrong `request_id` or a mismatched `input_sha256`. Worker killed. |
| `cancelled` | host | The import was cancelled. |
| `worker_absent` | host, per run | No worker at the configured path. |
| `worker_incompatible` | host, per run | No common protocol version, or the worker reports non-deterministic output (§2.16). r7.1: or the `hello` `sandbox` field disagrees with the host's spawn evidence. |
| `handshake_timeout` | host, per run (r4) | No valid `hello` reply within the handshake deadline; worker killed and reaped. |
| `shutdown_timeout` | host, per run (r4) | The worker did not exit within the shutdown deadline; worker killed and reaped. Item outcomes unchanged. |
| `reap_timeout` | host, per run (r4) | The killed child was not reaped within the reap deadline; a reaper thread keeps waiting. |

**Default limits.** §2.16 gives them. Configuration may lower any limit. Raising one above a compiled-in ceiling (proposal: 64 MiB input, 32 MiB output, 300 s) is refused.

**Fixtures.** All fixtures are generated in the repo.

| Fixture | Expected outcome |
|---|---|
| Text PDF, 5 pages | Text in page order; byte-identical on a second run. |
| PDF with a table | Table text present; table structure as Markdown where xberg gives it. |
| Scanned PDF, no text layer | Empty text and a `no_text_layer` note (3p A). |
| Corrupt PDF | `extract_error`; other items continue. |
| DOCX with headings, a list and a table | Headings, list and table survive as Markdown. |
| XLSX with two sheets | One table per sheet; sheet names in `fields`. |
| PPTX | Slide-ordered text. |
| Zip-bomb DOCX | `extract_error` or `output_too_large`, within the memory and time limits. |
| File over the input limit | `too_large`; not sent. |
| Test double: hangs | `timeout`; process group gone afterwards. |
| Test double: exits mid-request | `crashed`; next item gets a fresh worker. |
| Test double: floods stdout | `output_too_large`. |
| Test double: floods stderr | Excerpt capped at 64 KiB; item outcome unaffected. |
| Test double: wrong `input_sha256` | `protocol_error`. |
| Test double: protocol 99 only | `worker_incompatible` for the run. |
| Test double: never answers `hello` (r4) | `handshake_timeout` for the run; process group gone afterwards. |
| Test double: never reads stdin (r4) | `handshake_timeout`; the blocked `hello` write does not hang the host. |
| Test double: non-`hello` first frame (r4) | `protocol_error`; worker killed. |
| Test double: ignores `shutdown`, closed stdin and SIGTERM (r4) | Killed by SIGKILL; `shutdown_timeout`; reaped. |
| Test double: spawns a child that ignores SIGTERM | Child killed by the group SIGKILL and reaped. |
| Test double: prints its environment and working directory | Only allow-listed variables; empty directory. |

## Appendix C. Proposed source matrix for email and Google Docs (proposal) {#SPEC-OKF-01-C}

| | Gmail (`gmail`) | Google Docs (`gdocs`, a Drive locator for native Docs) |
|---|---|---|
| Adapter today | `crates/trusty-agents/src/tools/okg/gmail.rs`, `gapi.rs` | `drive.rs`, `gapi.rs`; native-Doc export is a gap (DOC-55 §2.3 item 2) |
| Selection | Account label, query, `after`/`before` window. Automated refresh: SENT only. Operator-initiated import: inbound allowed after the grant review (3l C). | Account label, folder ID with `recursive`, or document IDs |
| Item ID | Message ID | File ID |
| Revision | Message ID (immutable) | `version`, else `modifiedTime`, else volatile (`gapi.rs:396-407`) |
| Dates kept | `internalDate`, `Date` header | `modifiedTime`, `createdTime` (not requested today, `gapi.rs:36`) |
| Concept ID | `mail/<source-id>/<thread-id>/<message-id>` | `docs/<source-id>/<file-id>` |
| Structure | Thread directory plus a thread concept | Export `text/markdown`, else `text/html` (DOC-55 §4.6) |
| Body | Full text part, else HTML part, quoted text kept and marked (3k A). A snippet-only message writes no concept under the default (`degraded`). | Full export |
| Attachments | Listed by name, type and size; not extracted (3k A) | Embedded images dropped with a note |
| Complete listing | Never (windowed; DOC-55 C4) | Only when recursive and under the ceiling (`drive.rs:216-219`) |
| Deletions | Never tombstoned | `trashed` or absent on a complete listing → tombstone |
| Ceiling | 5,000 per call (`gmail.rs:36`) | 5,000 per call (`drive.rs:39`) |
| Chunk | 100 (`gmail.rs:44`) | 50 (`drive.rs:43`) |
| Trust label | `UntrustedExternal` (DOC-63 S-4.8) | `UntrustedExternal` |
| Grant | Existing Google Workspace credential for SENT mail. Inbound mail needs the capability-grant review first (3l C). | Capability-grant review first: native-Doc export is a new source type (item 10 B). |
| Extraction | Worker under the OS sandbox (item 9 B) | Worker under the OS sandbox (item 9 B) |
