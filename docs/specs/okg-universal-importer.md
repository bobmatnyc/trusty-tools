# DOC-55 — Universal OKG Importer: Any File Type, Any Connectable System, Assistant-Driven

**Status:** Draft  
**Subsystem:** trusty-kb — format extraction / connector framework / ingest engine; trusty-agents — connector adapters, assistant-facing tools, deterministic CLI surface  
**Owner:** Engineering (trusty-agents / trusty-kb) / Bob Matsuoka  
**Last-updated:** 2026-07-25  
**Spec ID:** `SPEC-OKGIMPORT-01~draft` … `SPEC-OKGIMPORT-07~draft` (DOC-55)  
**Builds on:** DOC-54 [Trusty Agents Product Specification](./trusty-agents-product-spec.md) §5.1 (the `[[stores]]` config leg); the shipped OKG ingestion engine (#3881 / PR #3883); the OKG store binding (#3878)  
**Related issues:** epic #3052 (Assistant M1), #3881 (ingestion layer, closed), #3892 (ingest→search gap, RESOLVED via remediation (a) — see §7), #3816 (declarative templates), #3837 (git-backed per-agent content store)

---

## 1. Executive Summary

The OKG ingestion engine shipped in PR #3883 already delivers the hard part: an
idempotent, additive, crash-convergent pipeline that turns fetched items into KB
entities behind a per-source append-only ledger. What it does **not** yet do is
read the world. It ingests UTF-8 text files, Gmail message bodies, and whatever
Drive hands back as text. A `.docx` in the corpus is skipped as binary. A `.xlsx`
is skipped as binary. A PDF is skipped as binary. Slack and Confluence have no
connector at all.

This spec closes that gap along two orthogonal axes and then names the feature
that makes both of them useful:

- **Axis 1 — formats (§4).** A pure, network-free *extraction layer* sitting
  immediately upstream of the existing `SourceItem` seam: bytes plus a type hint
  in, normalized Markdown-ish text plus provenance fields out. Extractors are
  deterministic functions; they never touch the network, never touch the ledger,
  and never know which connector produced their bytes.
- **Axis 2 — systems (§5).** A pluggable *connector framework*: a connector is
  exactly `list` + `fetch` over any reachable data system, and it inherits the
  ledger / watermark / idempotency contract **unchanged**. Adding Slack does not
  get to reinvent deduplication.
- **The defining feature — assistant-driven crawl (§6).** The assistant plans and
  drives multi-source imports conversationally. "Import my Q3 folder" decomposes
  into source registration, windowed ingestion, and progress reporting, with the
  assistant choosing the decomposition. This is what distinguishes an importer
  from a cron job, and it is the requirement Bob stated: *"any data system we can
  connect to, we should be able to crawl and import that data into an OKG store
  for an assistant, driven by the assistant."*

**On #3892 (ingest→search):** the first draft of this spec deliberately took no
position on whether an ingested entity reaches the bound search index. That
decision has since been made — remediation **(a)**, ingestion feeds the bound
index — and §7 now records the shipped contract every extractor and connector
inherits, rather than an open question.

---

## 2. SPEC-OKGIMPORT-01 — Ground Truth: What Already Exists {#SPEC-OKGIMPORT-01~draft}

This section is normative only in the sense that everything below **must not
break it**. It is a statement of the shipped design, not a proposal.

### 2.1 The seam

```
trusty-kb::okg                       (pure, deterministic, network-free)
  registry.rs   _sources/registry.toml   — SourceSpec rows, hand-editable TOML
  ledger.rs     _sources/<id>.jsonl      — append-only per-item journal
  ingest.rs     SourceItem -> entities + ledger lines (FETCHER-AGNOSTIC)
  docstore.rs   the in-crate filesystem fetcher
  policy.rs     DocStorePolicy — read-side confinement, re-checked every run

trusty-agents::tools::okg             (network, credentials)
  gapi.rs       Gmail/Drive JSON <-> SourceItem + paginated list calls
  {docstore,gmail,drive,sources}.rs    the four LLM-invocable tools
```

`SourceItem` (`crates/trusty-kb/src/okg/ingest.rs:40`) is the seam. It carries
`item_id`, `fingerprint`, `name`, `title`, `timestamp`, `body`, `fields`, and
`volatile`. Everything upstream of it is a fetcher; everything downstream is the
engine.

### 2.2 The invariants every extension inherits

| Invariant | Mechanism | Consequence for new code |
|---|---|---|
| **Idempotent** | `Ledger::is_current(item_id, fingerprint)` — one predicate decides skip vs write | A new connector supplies a stable `item_id` and an honest `fingerprint`; it writes no dedup logic of its own |
| **Additive** | `SourceRegistry::upsert` appends a new id, updates a known id in place, preserving `added_at` and the ledger | Registering a new source or widening a window is the same call; never a rebuild |
| **Crash-convergent** | Ledger journaled per item (`flush` + `sync_data`), entity written *before* its ledger line | A killed run re-converges; the reverse write order would lose data |
| **Fail-closed reads** | `DocStorePolicy::permit` applied inside `docstore::scan`, at the point the filesystem is touched — not at the tool boundary | A hand-edited `registry.toml` row pointing at `~/.ssh` is refused on every later run |
| **Deletions are never silent** | Absent items land in `IngestReport::missing`, or are tombstoned (`source_status: deleted`) with the body preserved | `detect_deletions` may be true only when the run enumerated the entire corpus |
| **Volatile items never freeze** | `SourceItem::volatile` bypasses the skip test entirely | A source with no revision signal re-writes each run; `put_entity`'s byte-compare keeps it cheap |
| **Chunked commit** | Network tools ingest in chunks (`CHUNK = 100` Gmail, `50` Drive) with detection off, then one `okg_sweep_deleted` | Partial progress survives a crash mid-backfill |
| **Model input is clamped** | `MAX_MESSAGES_CEILING` / `MAX_FILES_CEILING` = 5000 | Any new connector declares its own ceiling |

### 2.3 The gaps this spec addresses

1. `docstore::scan` rejects any non-UTF-8 file as binary
   (`crates/trusty-kb/src/okg/docstore.rs`) — `.docx`, `.xlsx`, `.pdf`, `.pptx`
   are all reported as `skipped_binary`.
2. `drive_content` returns `None` whenever the payload is base64
   (`crates/trusty-agents/src/tools/okg/gapi.rs:348`) — so a native Google Doc
   reaches us only if `trusty-gworkspace`'s wrapper happened to export it as
   text, and everything else silently yields nothing.
3. Gmail ingests the message body only; attachments are dropped entirely.
4. `Locator` (`registry.rs:48`) is a **closed** three-variant enum. A new system
   cannot be registered without editing `trusty-kb`.
5. The four `okg_*` tools are LLM-invoked only. There is no deterministic,
   scriptable invocation path — see §6.4.

---

## 3. SPEC-OKGIMPORT-02 — Scope, Non-Goals, and the Layering Rule {#SPEC-OKGIMPORT-02~draft}

### 3.1 The layering rule (normative)

> **`trusty-kb` stays pure, deterministic, and network-free.** Format extractors
> live in `trusty-kb` because they are pure functions over bytes. Connectors that
> need credentials or network live in `trusty-agents` (or a dedicated connector
> crate), because that is where the authenticated clients already are. They meet
> at `SourceItem`, exactly as Gmail and Drive already do.

This is not a stylistic preference. It is what makes every idempotency property
unit-testable with synthetic bytes and no network at all, and it is the reason
PR #3883's test suite could assert re-run/widen/edit/delete semantics without a
single live API call. A format extractor that reaches out to a conversion service
would destroy that property and is out of scope (§3.3).

### 3.2 In scope

- A format-extraction trait plus a first-wave extractor set (§4).
- A connector trait, its registry representation, and its policy/confinement
  obligations (§5).
- The assistant-driven crawl model and its deterministic counterpart (§6).
- Phased delivery and the ticket decomposition (§9).

### 3.3 Non-goals

- **No OCR, no ASR, no image understanding.** A scanned PDF with no text layer
  extracts to nothing and is reported as such. Model-based extraction is a
  separate future capability precisely because it is neither pure nor
  deterministic.
- **No network access inside an extractor.** Ever. Including "just to fetch a
  font" or "just to resolve an embedded link".
- **No new OAuth path.** Google connectors continue to reuse
  `trusty-gworkspace`'s `BaseClient` (#3883 constraint, restated). New providers
  reuse whatever authenticated client the workspace already owns, or the
  credential resolver (#2643), never a bespoke token store.
- **No parallel KB format.** Entity writes continue through
  `KbStore::put_entity`.
- **No re-litigation of #3892.** Settled as remediation (a); §7 records the
  contract this spec's connectors inherit.
- **No SQL / structured-database ingestion** (`cto.db`, `analytics.duckdb`) —
  explicitly out of scope in #3881 and still out of scope here. A spreadsheet
  read as a document (§4.4) is not the same thing as a database connector.
- **No GUI work** beyond rendering the progress reports §6.3 defines.

---

## 4. SPEC-OKGIMPORT-03 — The Format-Extraction Layer {#SPEC-OKGIMPORT-03~draft}

### 4.1 Position

```
   connector fetch  ──▶  raw bytes + type hint
                             │
                             ▼
                    ┌────────────────────┐
                    │  Extractor (PURE)  │   trusty-kb::okg::extract
                    └────────────────────┘
                             │
                             ▼   normalized text + extracted fields
                    ┌────────────────────┐
                    │    SourceItem      │   ← the existing seam, unchanged
                    └────────────────────┘
                             │
                             ▼
                      ingest engine (ledger, entities, tombstones)
```

Extraction happens **after** fetch and **before** `SourceItem` construction. The
engine is not modified at all by this axis.

### 4.2 The contract

An extractor is a pure function. Proposed shape (`trusty-kb::okg::extract`):

```rust
/// One extracted document: normalized text plus whatever structure survived.
pub struct Extracted {
    /// Markdown-ish plain text. Never base64, never raw XML.
    pub text: String,
    /// Provenance/structure fields merged into the entity frontmatter
    /// (`page_count`, `sheet_names`, `author`, `docx_title`, …).
    pub fields: BTreeMap<String, String>,
    /// Non-fatal notes ("3 embedded images dropped", "sheet 4 truncated").
    pub notes: Vec<String>,
}

pub trait Extractor: Send + Sync {
    /// Stable lowercase id, e.g. `docx`. Part of the fingerprint (§4.5).
    fn id(&self) -> &'static str;
    /// Monotonic version; bump whenever output for identical input changes.
    fn version(&self) -> u32;
    /// Whether this extractor claims the given MIME type / extension.
    fn claims(&self, hint: &TypeHint) -> bool;
    /// Extract. MUST be pure: no network, no filesystem, no clock, no RNG.
    fn extract(&self, bytes: &[u8], hint: &TypeHint) -> anyhow::Result<Extracted>;
}
```

Normative obligations:

- **E1 — Purity.** No I/O of any kind. Same bytes in, byte-identical `Extracted`
  out, on every machine and every run. This is testable and MUST be tested with
  a fixture-in / golden-out case per extractor.
- **E2 — Bounded.** Every extractor respects a byte ceiling analogous to
  `MAX_FILE_BYTES` (8 MiB) and an output-character ceiling; a zip-bomb `.xlsx`
  must fail, not exhaust memory. Decompression ratios are checked, not assumed.
- **E3 — Degrade, never abort.** An unparseable region yields a `notes` entry and
  the text that *was* recoverable. An extractor error is a per-item error
  (`IngestReport::errors`), never a run-ending one — matching
  `per_item_errors_do_not_abort_the_run`.
- **E4 — No secret leakage into notes.** `notes` and `fields` are written into a
  KB entity that may be committed (#3837). They carry structure, never content
  excerpts of anything the policy layer would have refused.
- **E5 — Chunking is the engine's job.** An extractor returns one `text`; the
  existing `DEFAULT_CHUNK_CHARS = 20_000` part-splitting continues to apply
  downstream, unchanged, so an 800-page PDF still becomes readable part-entities.

### 4.3 Type-hint resolution

`TypeHint` carries whatever the connector knows: a MIME type (Drive, Slack, and
Gmail all supply one), a filename extension, and the leading bytes for magic
sniffing. Resolution order is **MIME → extension → magic bytes**, first claim
wins, with a deterministic tiebreak on `Extractor::id()` so registration order
cannot change behavior. An unclaimed hint falls through to the existing UTF-8
text path, and then to `skipped_binary` — i.e. today's behavior is the floor.

### 4.4 First-wave extractors

| Extractor | Input | Output shape | Notes |
|---|---|---|---|
| `text` (exists) | UTF-8 text | verbatim | today's path, refactored behind the trait |
| `html` | `text/html`, `.html` | text with headings/links preserved as Markdown | already in `DEFAULT_EXTENSIONS` but currently ingested as raw markup |
| `docx` | OOXML wordprocessing | headings, paragraphs, lists, tables → Markdown | `fields`: core-properties (author, created) |
| `xlsx` | OOXML spreadsheet | one Markdown table per sheet, header row detected | `fields`: `sheet_names`, `row_count`; formulas → computed value, else formula text |
| `pdf` | PDF text layer | page-ordered text, page markers | no text layer ⇒ empty text + explicit note (§3.3) |
| `gdoc-export` | Drive export of a native Doc/Sheet/Slide | Markdown | see §4.6 — the *export choice* is the connector's, the parse is the extractor's |
| `pptx` (stretch) | OOXML presentation | slide-ordered title + body text | phase 3 |

### 4.5 Fingerprints and the extractor-version trap

**This is the subtle correctness issue in this axis, and it is normative.**

`docstore` fingerprints on `sha256` of the **raw bytes**. That is correct and
must stay correct — extracted text is derived, and hashing the derivation would
make the fingerprint depend on extractor behavior in an uncontrolled way.

But it creates a trap: improve the `docx` extractor, and every already-ingested
`.docx` keeps its old fingerprint, so `Ledger::is_current` skips it forever and
the improvement never reaches the corpus. The entity is silently frozen at its
first-seen extraction — the *exact* failure mode `SourceItem::volatile` was
introduced to prevent for Drive's unversioned files (code-critic HIGH 3 on
#3883).

**Rule: for any item whose body passed through a non-`text` extractor, the
fingerprint MUST be `sha256:<hex>/<extractor-id>:<extractor-version>`.**

Consequences, all intended:

- Bumping `Extractor::version()` invalidates exactly the items that extractor
  produced, and nothing else. They re-ingest on the next run; `put_entity`'s
  byte-compare means an extraction that did not actually change costs no write.
- The fingerprint remains a pure function of (bytes, extractor identity), so
  crash-convergence is preserved.
- `volatile` is **not** the mechanism here. Volatile means "re-write every run";
  this needs "re-write when the extractor changes", which is strictly weaker and
  cheaper.

### 4.6 Interaction with Drive exports

Native Google Docs/Sheets/Slides have no downloadable bytes — they are
*exported*, and the export format is a connector-side choice made at fetch time
(`files/export?mimeType=…`). The connector picks the export MIME; the extractor
parses whatever came back. Recommended exports: Docs → `text/markdown` where
available, else `text/html` (parsed by the `html` extractor, which preserves
structure that `text/plain` destroys); Sheets → `text/csv` per sheet, or `xlsx`
for multi-sheet fidelity; Slides → `text/plain` initially.

The current `drive_content` base64 short-circuit (§2.3) becomes: base64 payload
⇒ hand the decoded bytes to the extraction layer, and only report
`skipped_binary` when no extractor claims them.

---

## 5. SPEC-OKGIMPORT-04 — The Connector Framework {#SPEC-OKGIMPORT-04~draft}

### 5.1 What a connector is

> A connector is `list` + `fetch` over a reachable data system. Nothing more. It
> owns pagination, authentication, and its system's addressing; it owns **none**
> of dedup, tombstoning, entity writing, or watermarking.

```rust
pub trait Connector: Send + Sync {
    /// Stable lowercase kind, matching its registry sub-table name.
    fn kind(&self) -> &'static str;

    /// Enumerate item descriptors for a locator, honouring the window and the
    /// caller's ceiling. Returns descriptors, NOT bodies — this is what makes
    /// the ledger's pre-fetch consultation (exactly-once) possible.
    async fn list(&self, loc: &Value, window: &Window, max: usize)
        -> anyhow::Result<Listing>;

    /// Fetch bodies for a chunk of descriptors. Called only for items the
    /// ledger reports as not-current.
    async fn fetch(&self, ids: &[ItemRef]) -> anyhow::Result<Vec<FetchedBlob>>;

    /// Whether a completed `list` enumerated the ENTIRE corpus for this
    /// locator. Gates deletion detection; a windowed source returns false.
    fn enumerates_full_corpus(&self, loc: &Value) -> bool;

    /// Hard ceiling on items per invocation. Mirrors MAX_*_CEILING.
    fn ceiling(&self) -> usize { 5_000 }

    /// Chunk size for fetch+commit. Mirrors gmail.rs CHUNK / drive.rs CHUNK.
    fn chunk(&self) -> usize { 100 }
}
```

`FetchedBlob` is `{ bytes, type_hint, fields }` — it feeds §4's extractor and
then becomes a `SourceItem`. A connector whose system already returns text (e.g.
a Slack message) supplies `type_hint = text/plain` and the `text` extractor is a
pass-through.

### 5.2 Normative obligations

- **C1 — Stable `item_id`.** The system's own immutable id. If the system has
  none, the connector MUST derive one deterministically and document it. An
  `item_id` that changes across runs breaks every invariant at once.
- **C2 — Honest `fingerprint`.** A real revision signal when the system has one
  (Drive `version`, Confluence page version, Slack `edited.ts`). An immutable
  item may use its id as its fingerprint (Gmail's "pull exactly once, ever"). If
  there is genuinely no signal, set `volatile = true` — never invent a constant.
- **C3 — Ledger consulted before body fetch.** `list` returns descriptors so the
  exactly-once optimisation is real, not merely write-layer idempotency.
- **C4 — Deletion honesty.** `enumerates_full_corpus` returns true only when the
  listing genuinely covers everything the locator names. Gmail's windowed pull
  returns false, permanently. A connector that lies here tombstones a user's
  corpus.
- **C5 — Chunked commit + one sweep.** Ingest per chunk with
  `detect_deletions = false`, then call `okg_sweep_deleted` once with the full
  observed id set — the pattern `drive.rs` already implements.
- **C6 — Read-confinement is per connector.** `DocStorePolicy` is the filesystem
  instance of a general obligation: any locator field that a **model** can supply
  and that widens read scope must be gated by **operator** configuration,
  re-checked at the point of access, failing closed. Slack: an allowed-channel or
  allowed-workspace list. Confluence: allowed spaces. Filesystem: today's
  `[okg] docstore_roots`. The gate lives where the fetch happens, not at the tool
  boundary (the #3883 code-critic CRITICAL-2 lesson).
- **C7 — Soft errors are errors.** `BaseClient`-style APIs that return 403/404 as
  a 200-shaped body MUST be detected (`gapi::api_error`) and raised. Reporting
  "0 new items" for a permission failure is a data-loss bug, not a no-op.
- **C8 — No credential handling.** Tokens come from the existing authenticated
  client or the credential resolver (#2643). A connector never reads a token file.

### 5.3 Registry representation: opening the `Locator` enum

`Locator` is externally tagged, so the TOML sub-table name *is* the kind and the
two can never disagree — a property worth keeping. But it is closed: `Slack`
today means editing `trusty-kb`, which contradicts "any data system we can
connect to."

**Recommendation:** keep the three existing variants verbatim (they are
load-bearing, hand-editable, and already on disk) and add one open variant:

```toml
[[sources]]
id = "eng-handbook"
collection = "handbook"
added_at = "2026-07-25T00:00:00Z"

  [sources.locator.connector]
  kind = "confluence"
  params = { space = "ENG", updated_after = "2026-01-01" }
```

`Locator::Connector { kind: String, params: Json }` with `kind()` returning the
declared string. Rules: `kind` is slugged; an unknown `kind` at ingest time is a
clear "no connector registered for kind X" error, never a silent skip; `params`
is opaque to `trusty-kb` and validated by the connector. Registry
round-tripping, `upsert` additivity, and `save`'s write-if-changed behavior are
unchanged. Migrating `gmail`/`drive`/`docstore` into the open form is explicitly
**not** proposed — churn with no benefit.

### 5.4 Near-term connector roster

| Connector | Locator params | `item_id` | Fingerprint | Full corpus? | Notes |
|---|---|---|---|---|---|
| `docstore` (exists) | path, extensions, recursive | relative path | `sha256` (+ extractor tag, §4.5) | yes | gains every §4 extractor for free |
| `gmail` (exists) | query, after, before | message id | message id (immutable) | **no** | windowed |
| `gmail-attachments` | as gmail + MIME allow-list | `<msg-id>/<attachment-id>` | attachment id | no | first consumer of `docx`/`xlsx`/`pdf` |
| `drive` (exists) | folder_id, recursive | file id | `version:` / `modified:` | yes, iff fully recursive | export choice per §4.6 |
| `google-docs` | folder or doc ids | file id | `version:` | yes iff enumerable | a Drive locator specialised to native Docs |
| `slack` | workspace, channel allow-list, after | `<channel>/<ts>` | `edited.ts` else `ts` | per-channel yes, windowed no | thread replies are items; files are attachments |
| `confluence` | base URL, space allow-list, updated_after | page id | page `version.number` | per-space yes | native storage format → `html` extractor |
| `filesystem-formats` | — | — | — | — | not a connector; the `docstore` connector + §4 extractors |

Ordering rationale: `gmail-attachments` and `google-docs` come first because
they need no new auth and immediately exercise the extraction layer against real
user data. Slack and Confluence follow, and each needs its own C6 policy gate.

---

## 6. SPEC-OKGIMPORT-05 — Assistant-Driven Crawl {#SPEC-OKGIMPORT-05~draft}

### 6.1 The requirement

Bob, 2026-07-25: *"any data system we can connect to, we should be able to crawl
and import that data into an OKG store for an assistant, **driven by the
assistant**."*

The emphasis is the feature. A cron-driven importer is a solved, boring problem.
The claim here is that the **assistant** decomposes a vague human request into a
concrete multi-source import plan, executes it incrementally, reports progress in
the conversation, and adapts when a step returns something unexpected.

### 6.2 The loop

```
"import my Q3 folder"
        │
        ▼
  ┌─ PLAN ──────────────────────────────────────────────────────┐
  │ resolve "Q3 folder" (drive search / prior context / ask)     │
  │ inspect: 340 files — 120 native Docs, 90 docx, 40 xlsx, 90 … │
  │ propose: 1 drive source + 1 window, est. 4 chunked passes    │
  └──────────────────────────────────────────────────────────────┘
        │  user confirms (or edits the plan)
        ▼
  ┌─ REGISTER ──────────────────────────────────────────────────┐
  │ okg_register — additive upsert into _sources/registry.toml    │
  └──────────────────────────────────────────────────────────────┘
        │
        ▼
  ┌─ INGEST (windowed, resumable) ──────────────────────────────┐
  │ repeat: ingest a bounded slice; ledger commits per chunk     │
  │ report after each: N ingested / M skipped / E errors         │
  └──────────────────────────────────────────────────────────────┘
        │
        ▼
  ┌─ REPORT + ADAPT ────────────────────────────────────────────┐
  │ "38 files had no text layer — want me to list them?"         │
  │ "Coverage now 2026-07-01 → 2026-09-30. Reach further back?"  │
  └──────────────────────────────────────────────────────────────┘
```

Everything except PLAN and ADAPT is mechanical and already exists in some form.
The assistant's genuine contribution is: resolving fuzzy references to concrete
locators, choosing a decomposition that fits within the per-call ceilings,
deciding what deserves a question versus a note, and knowing when to stop.

### 6.3 What the platform must add

- **A `okg_plan_import` tool (read-only).** Given a fuzzy target, returns a
  *proposed* plan as data: the resolved locators, an item-count estimate, a
  format breakdown, the number of passes implied by the ceilings, and any policy
  refusals. It registers nothing and ingests nothing. This is what makes the
  conversation possible before the writes start.
- **A `okg_register` tool split out from the ingest tools.** Registration is
  currently folded into ingestion on purpose (#3883: "point at this directory"
  and "reach further back" are the same call). That remains true for the
  single-shot path, but a plan-then-execute flow needs to register a *set* of
  sources before running any of them. Both paths must funnel through the same
  `okg_register_source`.
- **Resumable progress.** `IngestReport` already merges chunk reports and carries
  a `watermark`. A multi-pass crawl needs the same at the *plan* level: which
  sources in this plan are complete, which are partial, where the next pass
  resumes. Recommendation: derive it from the existing per-source ledgers
  (`okg_sources` already exposes coverage) rather than introducing a second piece
  of durable state that can disagree with the ledger.
- **Progress a human can watch.** The GUI renders the per-chunk report stream;
  no new durable state.

### 6.4 The determinism gap (must be stated, not papered over)

Today the entire OKG surface is **LLM-invoked**. `okg_tools()`
(`crates/trusty-agents/src/tools/okg/mod.rs:53`) returns four `ToolExecutor`s,
reachable only through a model turn. Consequences:

- A crawl cannot be scripted, cron'd, or run in CI.
- A crawl cannot be reproduced exactly — the same request may decompose
  differently on a different turn.
- Debugging an ingestion is debugging a conversation.
- A long backfill burns model tokens on mechanical paging.

**This is a real gap and this spec does not pretend the assistant-driven model
removes it.** The two are complements: the assistant is the right driver for
*"import my Q3 folder"*; it is the wrong driver for *"re-run the nightly
backfill."*

**Recommendation: a `tagent okg` subcommand** as the deterministic invocation
surface — `tagent okg sources`, `tagent okg register`, `tagent okg ingest <id>
[--max N] [--since DATE]`, `tagent okg plan <target> --json`. Same engine, same
registry, same ledgers, same policy gates; no model in the loop. The assistant's
tools then become thin wrappers over the same internal API the subcommand calls,
so there is exactly one implementation and the CLI is the reproducible one. A
crawl plan produced by `okg_plan_import` should be serialisable to something
`tagent okg` can execute verbatim — that is what makes an assistant-planned
import auditable and re-runnable.

**Open question (owner sign-off):** is the CLI in scope for the first wave, or
does it follow after the extraction layer lands? §9 sequences it as phase 2 on
the assumption that the first real backfill will immediately want it.

---

## 7. SPEC-OKGIMPORT-06 — Position Relative to the Ingest→Search Gap (#3892) {#SPEC-OKGIMPORT-06~draft}

### 7.1 The decision, now settled

#3892 documented, with live evidence, that an OKG store's two facets did not
meet: `okg_ingest_*` writes into
`${KB_KNOWLEDGE_DIR:-$HOME/.trusty-agents/knowledge}/<agent>`, while the
`[[stores]]` binding's `index` is a trusty-search index built over an unrelated
root; `okg://<agent>` was never resolved to a filesystem path at all. A user could
run an ingest, get a truthful "N entities ingested", and still get nothing from
`vector_search`. Two coherent remediations were on the table — **(a)** ingest also
feeds the bound index, or **(b)** the store's index is (re)built over the KB tree
— and (b) collided with a deliberate operator choice (the 200,090-chunk
`bob-duetto/cto` index).

**The owner chose (a)** (2026-07-25). Ingestion now feeds the bound index, and the
operator's index-root configuration is left untouched. Shipped shape:

- `trusty_kb::okg::index_journal` — a second append-only journal per source,
  `_sources/<id>.index.jsonl`, recording what reached the index and at what
  content hash, plus `KbStore::okg_index_backlog`, the pure reconcile that diffs
  the item ledger against it. The ledger is untouched: it remains the record of
  KB-tree truth.
- `trusty_agents::stores::index_feed` — the network half (`IndexFeed` seam,
  `HttpIndexFeed` over `POST /indexes/{id}/index-file` and `/remove-file`) and
  the drive loop. It lives in `trusty-agents` for the same reason the
  Gmail/Drive fetchers do: `trusty-kb` stays deterministic and network-free.
- `trusty_agents::stores::binding` — `okg://<agent>` finally RESOLVES, to
  `<knowledge_dir>/<slug(agent)>`, and a push is refused when the binding's tree
  is not the tree that was written. The naming coincidence #3892 named is now a
  checked correspondence.

### 7.2 The resulting contract (normative)

Every connector and every extractor inherits this; none of them may weaken it.

1. **The tree write is the durable record; the index push is not.** A ledger
   line is never gated on a reachable daemon. A search daemon that is down,
   slow, or 500ing cannot lose ingestion work or make it non-convergent. This is
   the position §7.2.4 of the draft took and it is what shipped: the ledger is
   KB-tree truth and is NOT overloaded with index state.
2. **Success is never claimed falsely.** Every ingest reports `index.indexed`,
   `index.removed`, and `index.pending` — the count of entities that are in the
   tree and NOT searchable — plus a `reason` when nothing could be fed at all
   (no binding, daemon undiscoverable, tree/index mismatch). "N entities
   ingested" alone is no longer a complete answer, and no surface is allowed to
   imply it is.
3. **Push, then record.** A journal line is appended only after the daemon
   acknowledges the write. A crash in between costs one redundant re-push (the
   push is an upsert keyed by the entity file path), never a permanently
   unindexed entity — the mirror of the engine's entity-before-ledger ordering.
4. **Every run reconciles the whole backlog, not just what it ingested.** An
   entity the engine skipped as unchanged but which never reached the index is
   pushed by the next run. This is what makes a failed push self-healing rather
   than permanent, and it is why index state cannot live in the ledger: the
   ledger's own skip decision would hide it forever.
5. **A changed entity is withdrawn before it is re-pushed**, and a tombstoned one
   is withdrawn outright. trusty-search chunks by content, so re-pushing without
   withdrawing would leave the old revision searchable alongside the new one.
6. **An index that cannot serve the tree is refused, not fed.** trusty-search
   post-filters every search result whose file path escapes the index root
   (issues #64 / #541) — verified live: the push succeeds, the chunks are in the
   corpus, and `search` returns nothing. So an OKG store's bound index is usable
   only when its root CONTAINS the tree. The feed asks the daemon for the index
   root first and, when it does not, reports that with the remediation rather
   than manufacturing a fresh silent failure. This is also why (a) never has to
   re-point an existing index root: a binding whose index cannot serve its tree
   is a configuration error, and it is now a loud one. (For `cto-assistant`
   specifically: the `bob-duetto/cto` index remains a separate raw-search layer;
   making its OKG tree searchable means binding an index registered over that
   tree.)
7. **The backlog is visible where an operator looks.** `okg_sources` reports
   per-source `index.synced` / `index.pending` and names the bound index;
   `GET /api/agents/:name/stores` reports the resolved `tree_path` plus
   `pending_index` / `synced_index`. "Ingested but not yet searchable" is a
   reportable state, never an invisible one.
8. **The importer remains neutral in shape.** The extraction layer is upstream of
   `SourceItem`; the connector framework ends at the ingest engine. The index
   push attaches at exactly one place — the ingest tool's tail, over the source's
   backlog — so a new connector inherits it with no per-connector work.

---

## 8. SPEC-OKGIMPORT-07 — Security and Failure Posture {#SPEC-OKGIMPORT-07~draft}

1. **Every new read scope is operator configuration.** C6 (§5.2). The model may
   name *what* to import; the operator decides *what is reachable*. Defaults are
   useful but never `/` — the `[okg] docstore_roots`-defaults-to-`$HOME`-minus-
   hidden-paths pattern is the template.
2. **Gates are re-checked at access time.** A registry row is data, and data can
   be hand-edited or pre-date the gate.
3. **Extractors are an attack surface.** They parse untrusted, user-supplied
   binary formats. Obligations: memory ceilings and decompression-ratio checks
   (E2); no shelling out to an external converter; prefer pure-Rust parsers with
   a maintained dependency; extraction failures are per-item, never fatal.
4. **Extracted content inherits the corpus's sensitivity.** A `.docx` from a
   restricted Drive folder becomes a plaintext KB entity that may be committed by
   #3837's git-backed store. Store confinement and `.gitignore` posture are the
   controlling mechanisms; this spec adds no new exemption.
5. **Fail closed on ambiguity.** An unreadable path segment, an undeterminable
   type, an unknown connector kind: refuse and report. Never "assume text".

---

## 9. Phased Delivery

Each phase is independently shippable and leaves the tree in a working state.

**Phase 1 — Extraction layer + first extractors.**
`trusty-kb::okg::extract` (trait, `TypeHint` resolution, ceilings, golden-fixture
harness); `text` refactored behind the trait; `html`, `docx`, `xlsx`, `pdf`;
fingerprint extractor-tagging (§4.5); `docstore::scan` routes through extraction
before declaring a file binary. Net effect: **a `.docx` in a doc store ingests.**
No new connector, no new auth, no network.

**Phase 2 — Connector framework + deterministic surface.**
The `Connector` trait; `Locator::Connector` open variant; existing fetchers
refactored behind the trait *without* changing their registry rows; per-connector
policy hook (C6); `tagent okg` subcommand (§6.4). Net effect: **adding a system
no longer means editing the engine, and a crawl is scriptable.**

**Phase 3 — Google surface completion.**
`drive_content` routes base64 through extraction (§4.6); export-format selection
for native Docs/Sheets/Slides; `gmail-attachments` connector. Net effect: **the
data the user already has in Google lands in full fidelity.** *The #3892 gate is
cleared — remediation (a) shipped, and every connector inherits the §7.2
contract for free.*

**Phase 4 — Assistant-driven crawl.**
`okg_plan_import`; `okg_register` split; plan-level progress derived from
ledgers; GUI progress rendering; plan → `tagent okg` serialisation.

**Phase 5 — Beyond Google (stretch).**
`slack`, `confluence`, `pptx`. Each carries its own C6 policy gate and its own
ticket.

---

## 10. Open Questions (owner sign-off)

| # | Question | Recommendation |
|---|---|---|
| Q1 | Is `tagent okg` (§6.4) in the first wave or a follow-up? | Phase 2 — the first real backfill will want it immediately |
| Q2 | ~~#3892 remediation (a) or (b)?~~ | **Answered 2026-07-25: (a).** Shipped; §7 records the contract |
| Q3 | Do extracted-format entities need a distinguishing marker in frontmatter (`extracted_by: docx@2`)? | Yes — it is free, and it makes an extractor-version re-ingest auditable |
| Q4 | PDF parser dependency choice (pure-Rust vs. bindings) | Pure-Rust, per §8.3; the specific crate is an implementation ticket |
| Q5 | Should `xlsx` become a *structured* source (rows as entities) rather than a document? | No — that is the SQL/database gap (#3881 out-of-scope), tracked separately |

---

## 11. References

- `crates/trusty-kb/src/okg/` — engine: `mod.rs`, `ingest.rs`, `registry.rs`,
  `ledger.rs`, `docstore.rs`, `policy.rs`
- `crates/trusty-agents/src/tools/okg/` — `gapi.rs`, `docstore.rs`, `gmail.rs`,
  `drive.rs`, `sources.rs`, `config.rs`
- `crates/trusty-agents/src/stores/` — the `[[stores]]` binding (#3878)
- [DOC-54 Trusty Agents Product Specification](./trusty-agents-product-spec.md) §5.1
- Issues: #3052 (epic), #3881 / PR #3883 (engine), #3878 (binding), #3892
  (ingest→search gap), #3816 (templates), #3837 (git-backed content store),
  #2643 (credential resolver)

---

## 12. Change Log

| Date | Change |
|---|---|
| 2026-07-25 | Initial draft — extraction layer, connector framework, assistant-driven crawl, #3892 position |
| 2026-07-25 | §7 rewritten: #3892 settled as remediation (a); the open position becomes the shipped ingest→index contract |
