# Real memory validation interface — design only

Date: 2026-09-17. Source findings: `/tmp/real-memory-research.md`.
Experiment: `experiments/trusty-memory-real-validation`; public documentation: `docs/research/trusty-memory-real-validation-2026-09-17`.

## Scope and reuse

Use a separate read-only Rust exporter plus two small Python modules (adapter/runner); target under 400 Python production lines and approximately 100 Rust lines before tests. Reuse `plan_bridge`, `Task`, `Source`, `Fact`, `DerivedStore`, `build_scoped_index`, frozen `run_arm`, `packet`, `RustHelper`, and the existing tokenizer. No service framework, daemon changes, embeddings, predicate normalization, grammar changes, or updates to previous experiments.

Three arms only: `raw_bm25`, `old_combined`, `structured_plan`. The latter two call frozen `plan_experiment.run_arm` unchanged. Raw BM25 searches the same `index.claim_id`, limit 20, builds `Selection` in native ranking order, and passes it to the same packer. All use budgets 128 and 256; one warmup and three measured repetitions; identical snapshot/task context. Report end-to-end comparisons, not a matched-candidate graph ablation: frozen arms have different candidate acquisition. An optional shared-pool diagnostic is out of this implementation's scope.

## Export boundary

CLI: `memory_snapshot_export --database <existing-path> --private-output <new-file> --max-rows <positive-bound>`; no implicit database discovery. The parent supplies a private output directory outside every Git checkout. Require `create_new`, mode 0600, refuse output inside any Git worktree, never print content to stdout. Print only row counts and output digest. Do not overwrite files. On failure, remove only this invocation's incomplete output.

Open via public `ReadOnlyRedb::open`, then one read transaction. Do not construct `KgStoreRedb`, `KnowledgeGraph`, `PalaceHandle`, an embedder, or AppState on the live path. Read `DRAWERS` and `TRIPLES` completely within the declared bound; exceeding it is an error, never silent truncation. Missing tables fail clearly unless explicitly recorded as absent and approved as an empty initialized corpus. Decode current `DrawerRecord` and `TripleValue`, decode active and `hist:` keys using existing public functions. Current-record decoding failure aborts with the row key digest and count, not raw row content. The parent decides a separate legacy decoder path if real data requires one.

Export JSON envelope:

```python
class ExportCounts(TypedDict):
    read: int
    decoded: int
    errors: int

class DrawerRow(TypedDict):
    key_hex: str
    record: dict[str, JSON]  # exact serialized DrawerRecord, validated by adapter

class TripleRow(TypedDict):
    key_hex: str
    row_kind: Literal['active', 'history']
    subject: str
    predicate: str
    object: str
    valid_from_ms: int
    valid_to_ms: int | None
    confidence: float
    provenance: str | None

class Snapshot(TypedDict):
    version: Literal['memory-real-snapshot-v1']
    captured_at_ms: int
    drawers: list[DrawerRow]
    triples: list[TripleRow]
    counts: dict[str, ExportCounts]
```

Invariants: each table `read == decoded`, `errors == 0`, unique raw keys, total rows <= bound; decoded key object equals value object or fail. Preserve timestamps, provenance, confidence, completed_at and fact_key exactly in private export. A transaction is a coherent database view; file hashes during a concurrent writer are not transaction hashes. Manifest hashes the resulting export bytes. A live before/after file difference alone does not prove exporter mutation.

## Typed Python boundaries

```python
Arm = Literal['raw_bm25', 'old_combined', 'structured_plan']
JudgmentStatus = Literal['positive', 'negative', 'unavailable', 'ambiguous']

@dataclass(frozen=True)
class HistoricalPrompt:
    query_id: str                 # opaque sample ID (real-001 etc.); never passed to policy
    prompt: str                   # exact extracted historical user text
    logged_at: str                # provenance only; not query as_of
    scope: str
    log_file: str
    log_line: int
    log_file_digest: str

@dataclass(frozen=True)
class Judgment:
    query_id: str
    status: JudgmentStatus
    required: tuple[tuple[str, ...], ...]
    supporting_spans: tuple[tuple[str, int, int], ...] # drawer source ID, byte start/end
    acceptable: tuple[str, ...]
    forbidden: tuple[str, ...]
    corroboration: tuple[tuple[str, str], ...] # graph evidence ID, drawer evidence ID
    rationale: str               # private only

@dataclass(frozen=True)
class AdaptedCorpus:
    sources: tuple[Source, ...]
    evidence_metadata: Mapping[str, Mapping[str, JSON]]
    counts: Mapping[str, int]


def read_snapshot(path: Path, max_rows: int) -> Snapshot: ...
def adapt_snapshot(snapshot: Snapshot, scope: str, encoding: Encoding) -> AdaptedCorpus: ...
def read_frozen_sample(path: Path, expected_digest: str) -> tuple[HistoricalPrompt, ...]: ...
def run_case(task: Task, arm: Arm, index: ScopedIndex, helper: RustHelper, budget: int, encoding: Encoding) -> Mapping[str, JSON]: ...
def score_cases(cases: Sequence[Mapping[str, JSON]], gold: Mapping[str, Judgment]) -> Mapping[str, JSON]: ...
def main() -> None: ...
```

Use existing `JSON` recursive type, no production `Any`. Validate external dictionaries once. Inject helper and encoding; module import must not open files, connect, build models, or inspect private paths.

## Adaptation contract

1. Each drawer is a source with content verbatim. Use opaque `drawer:<key>` subject, free string predicate `memory_text`, and exact contiguous content spans as evidence. Split text deterministically into maximum 80-token chunks at valid UTF-8 boundaries, without overlap or dropped non-whitespace bytes. No invented semantic fact extraction. Preserve full source body and map each claim back to exact byte offsets. Do not use drawer titles/tags as asserted relation subjects.
2. Each triple row is its own source with a deterministic rendered assertion body. Its spans refer only to that rendering, explicitly `evidence_kind=kg_record`, never to an originating drawer. Preserve native predicate spelling. Keep history distinct by raw key. `object_entity` is set only when object exactly equals a subject among currently eligible graph rows; otherwise null. Count both decisions. This is a conservative heuristic, not a stored entity/literal type.
3. Preserve all raw millisecond fields in metadata. Perform current eligibility using the raw snapshot timestamp: drawer expiry and triple validity. Only then construct compatible Source/Fact timestamps for the frozen runtime. Set synthetic source observed_at to the snapshot instant, verified_at null, revision 1 documented as snapshot revision rather than real source version; keep real created_at separately. Do not claim historical freshness correctness. Do not invent supersession across drawer fact_key values or infer standing=true from importance.
4. Current snapshot eligibility exclusions must be counted by cause. Scope is the selected palace, not room. Source IDs are opaque digests and collision checked. Empty drawers are counted, not invented as facts. Evidence-kind counts, token totals, unsupported predicate counts, and graph provenance/corroboration coverage are mandatory.

Opaque drawer facts will often be unusable by the frozen entity/relation selectors. Report this explicitly as a storage/query interface mismatch. It is not evidence that useful drawer information does not exist. The raw control evaluates the same adapted evidence corpus and is not the installed production BM25 implementation's complete behavior.

## Sampling, judgments, and reporting

The parent has already frozen `/Users/masa/trusty-search-experiment/private/real-memory-2026-09-17/sample.json`: 32 exact prompts selected by fixed-salt SHA256 from 607 unique eligible prompts, with 5–200-word limits, markup-start exclusion, and hook JSON unwrapping. Reuse this sample; do not implement a new sampler, change exclusions, or read prompt content during design/implementation. Read its manifest/digest at execution only. Report the parent's recorded sampling/exclusion counts and limitations. No output-based replacement of prompts is allowed.

Independent judge sees sampled prompts and current corpus, never arm outputs. Label positive only when required evidence is independently established. Negative means a clear request requiring no memory (or an independently justified empty-answer target), not merely a failed search. Missing evidence is unavailable; underspecified/context-dependent requests are ambiguous. Do not count either as successful abstention. Graph claims need drawer corroboration where possible; unresolved source support is separately counted and excluded from precision/abstention claims. Preserve judgments privately and hash them before retrieval.

Gold includes relevant drawer source IDs plus exact supporting quote byte spans, validated against the frozen source bytes. Map those spans deterministically to overlapping chunks. Report note-level recall separately from required supporting-span coverage: merely returning another chunk from the same drawer does not count as answer support. A support span is complete only when the emitted chunks cover every byte of that span. For mixed-content chunks, report supporting-span byte coverage and emitted relevant-chunk fraction separately; do not label a byte-overlap fraction semantic fact precision. KG precision uses separately corroborated graph evidence IDs.

Metrics across all 32: parser status counts, entity resolution, supported native predicates, ready-request rate, candidate and emitted evidence-kind counts, bounded execution counters, latency and tokens. On independently judged positives: candidate, selected, packed required-group coverage; exact evidence-unit precision with denominator shown; full-query completion. On judged negatives: empty-packet rate. Show unavailable/ambiguous/unresolved-corroboration counts separately. Report graph-only additions supported by gold as a descriptive count, never causal graph gain. Candidate caps and plan limits remain frozen; report bound hits and corpus sizes, because 20 lexical hits/32 emitted graph facts may be inadequate at live scale.

Private CLI requires explicit `--snapshot`, `--prompts`, `--gold`, `--private-output`, `--helper`; every content-bearing output remains outside Git. Public report is separately written from reviewed aggregate counts only. No prompt samples, entity names, paths inside drawers, credentials, raw packet text, or per-query traces enter commits or tickets.

## Verification contracts

Exporter: create a temporary redb with drawers plus active/history triples; hash before/after and verify unchanged; count/decode integrity; missing path must not create a database; legacy/malformed row must fail without a partial final artifact; over-limit must fail; output-inside-worktree must fail. No live mutation test. Run the actual exporter against the authorized real database after these pass.

Adapter/runner: UTF-8 exact span round-trip including long non-ASCII content; metadata/time/predicate preservation and strict counts; frozen sample digest/count enforcement; distinct unavailable/ambiguous labels excluded from abstention; frozen arms called unchanged; real helper entry-point smoke test; repeated packet signatures equal; gold unavailable to retrieval functions. Verify no old experiment hash changes. Strict mypy over new modules and focused tests suffice for adapter scope; Rust example build/test gates cover exporter changes. No performance assertion without measured results.

## Preparation boundary

CLI mode `prepare` requires snapshot, frozen prompts and a fresh private output directory, and emits adapted source/evidence/span metadata for independent judging without building retrieval indexes or ranking. Mode `evaluate` additionally requires frozen gold and the helper. `prepare` must not call any arm. The existing sample ordering is SHA256 of memory-real-validation-v1, NUL, exact prompt (without scope in the hash). Retain sample IDs and log file/line/digest locators exactly; do not fabricate a raw-record digest.

## Scoring corrections before evaluation

Gold requires nonempty `required` groups for every positive. Each group contains alternative evidence IDs that independently satisfy one mandatory requirement. Multi-chunk required quotes need a group per required chunk. Relevant drawer IDs must overlap exact supporting drawer spans. Unknown required IDs, contradictory forbidden IDs, and positives with only optional acceptable IDs fail validation. Every positive contributes a numeric candidate/selected/packed group coverage and a binary complete-query result.

Report drawer relevant-chunk fraction and corroborated KG precision separately. Unresolved graph records stay outside judged precision denominators and have explicit counts. Raw total emitted counts remain descriptive. Alongside complete supporting-span coverage, report union supporting-byte coverage per source at candidate, selected and packed stages; overlapping quotes do not count twice. Group completion expresses judged requirements; byte coverage reports the distinct drawer-support view.

Every packet passes the inherited independent source/span/rendering/budget validator outside its timed interval. No gold enters packet validation or retrieval. The actual runner takes canonical source records explicitly for this check.

## Legacy decoder compatibility after the first export attempt

The first read-only export failed on a legacy drawer before creating an output. The exporter now mirrors the canonical stored drawer layouts from `kg_redb/types.rs`: legacy (six fields), pre-task (eight), pre-fact-key (nine), and current (ten). Every decode must reserialize byte-for-byte to the original row, so unknown trailing data fails. The exporter never opens the live database for writing.

Rows contain `decode_version` and `absent_fields`; newly introduced missing fields are represented as null in the adapted current-shaped record, with absence explicitly retained. Top-level `drawer_decode_versions` reports counts without changing the existing per-table count structure. Preparation preserves both row metadata and version totals; evaluation provenance includes the totals. Present fields, including completed_at and expiry, remain unchanged.
