# Lexical fallback and coherent passages: Stage 2 interface

Status: proposed, 2026-09-17. Parent approval is required before Stage 3. This is a bounded research interface for #8246, not a production API or a catalogued product specification. No implementation bodies or private sample/gold contents appear here.

Inputs: `/tmp/memory-passage-research.md` and `docs/research/trusty-memory-passages-2026-09-17/protocol.md`. Paths below are relative to `/Users/masa/trusty-search-experiment/worktree`.

## Ownership, reuse and limits

New files only: `experiments/trusty-memory-passages/passage_policy.py`, `passage_evaluate.py`, `test_passage.py`, and a short README. Target 500–700 source SLOC total, each source file below 500. If typed records and runner cannot fit those bounds, report the required split before adding a third source module. Reuse sibling modules through their existing explicit import-path bridge pattern; do not change their globals or files.

Reuse `real_adapter.read_snapshot`, `adapt_snapshot`, `read_frozen_sample`, private writers and hashes; `plan_index.build_scoped_index` and `validate_context`; original `Task`, `Source`, `Fact`, `Evidence`, `Selection`, `Packet`, `RustHelper`, offline encoding; `relevance.packet`, `retrieval.format_claims`, and `packet_integrity.validate_packet`. Reuse `real_evaluate.merge_intervals`, but not its chunk-ID scoring or gold acceptance semantics. Dependency injection consists of explicit helper/index/encoding arguments. No service container, repository hierarchy, abstract base class, or custom plugin interface is needed.

The existing frozen helper is `/Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe`; Python is `/Users/masa/trusty-search-experiment/venv/bin/python`. Existing installed `tiktoken`, pytest and mypy are sufficient. No installation or dependency version changes are authorized.

The implementation will not build Rust, initialize embedding models, use network services, export live memory, write live memory, mutate prior experiments/results, tune against either held-out set, change the exact query sent to native search, apply typed intent parsing, or treat graph metadata as note support. It will not claim historical replay, comprehensive precision, or production readiness.

## Frozen comparison and numeric policy

All arms use the same eligible original mixed fixed-chunk projection (`ScopedIndex.claim_id`), exact `Task.prompt`, and native search limit 20. The existing adapter's 80-token chunking remains unchanged. One search request returns the candidate list for each case; no alternative query or projection is introduced. Candidate order is native order. Duplicate/unknown IDs are errors. The corpus must contain no standing prelude facts, as guaranteed by the current adapter; unexpected standing facts fail validation rather than silently alter the comparison.

| Arm | Selection | Allocation |
| --- | --- | --- |
| `raw_bm25` | Every native candidate, unchanged mixed drawer/KG control | Original `relevance.packet` and fixed chunks |
| `lexical_fixed` | Frozen lexical drawer selector, maximum 8 seeds | Original `relevance.packet` and fixed chunks |
| `lexical_passages` | Exactly the same selected seed IDs and order as `lexical_fixed` | Registered coherent source passages |

Removing KG from emitted answer text is explicitly part of the selector intervention. Raw-to-fixed measures this filtering plus lexical abstention and the seed cap. Fixed-to-passages measures coherent allocation, including additional adjacent source bytes and changed evidence-identifier rendering overhead. It is not a candidate retrieval improvement. Exact canonical rendering remains identical in form across arms; its complete overhead counts toward budgets.

Policy ID: `memory-passages-v1`. Budgets: 128 and 256 tokens. Maximum prompt size: 65,536 UTF-8 bytes; larger input is an explicit error, never truncation. Ordinary matching tokens are Unicode word sequences matched by `\w+`, casefolded, distinct, length at least 3. Exclude this frozen general stop set: `about after again also and are before can could does for from have how into its not now our please should than that the their them then there these they this those through was were what when where which who why will with would you your`. No stemming, synonyms, prompt-specific rules, or learned weights.

Document frequency is measured once over eligible original drawer bodies, one count per source per term. N is the number of eligible drawer sources. A term is informative when its document frequency is positive and at most 10% of N. Weight is natural-log `log((N+1)/(df+1))+1`. Ordinary support requires at least 2 distinct informative query terms present in the candidate claim and at least 25% of total informative query-term weight. Query terms absent from the corpus do not contribute weight. The threshold is intentionally a policy hypothesis, not a calibrated probability.

An exact technical anchor can independently establish support. Anchors are whitespace-delimited strings containing a slash or `::`, issue references of the form `#` followed by decimal digits, and nonempty single-line backtick contents of at most 128 characters. Strip only exterior sentence delimiters `,;!?()[]{}\"'` from slash/qualified candidates; do not strip `/`, `.`, `_`, `-`, `#`, `:` or internal characters. Anchors must contain at least one alphanumeric character and at least 3 characters total. Backtick contents may include spaces and retain exact bytes. Matching is case-sensitive literal occurrence with no adjacent Unicode word character on either side. An anchor qualifies only if its exact source-level document frequency is positive and at most 10% of N. Count source frequency lazily per observed query anchor without mutating source text or search queries. Technical anchors after ordinary prose are retained because the entire bounded prompt is inspected.

Selection includes qualifying drawer candidates in native order, up to 8, without reranking. Rejections distinguish `kg_record`, `below_threshold`, and `seed_cap`. Query-level reasons distinguish `empty_query` (no matching terms or anchors), `no_candidates`, `below_threshold`, and `selected`; packing adds `budget`, `overlap`, `oversized_unit`, and `no_unit`. Empty packet and selector abstention are distinct diagnostics.

## Semantic units and exact source authority

Units and windows are predefined from eligible drawer bodies without prompts, candidate ranks, or gold. UTF-8 byte intervals use half-open `[start_byte,end_byte)` bounds. Raw CRLF, blank lines, whitespace, and source bodies are retained. Headings are lines beginning with 1–6 `#` followed by whitespace. A heading starts a new section and is a standalone unit. A paragraph is consecutive nonblank nonheading prose lines. A list item begins with optional indentation then `-`, `*`, `+`, or digits followed by `.` or `)`, and whitespace; continuation lines belong to that item until the next item or structural boundary. A fence begins with optional indentation and at least 3 backticks or tildes; it ends at the same marker with at least the opening length and only trailing whitespace. All interior fence bytes form one atomic unit, including list/heading-like lines. An unclosed fence extends to source end. No sentence splitting is attempted, avoiding punctuation rules for code and paths. Oversized prose/list items remain atomic and may be skipped.

Amendment approved before implementation: oversized prose/list units split at sentence punctuation (`.`, `?`, `!`) followed by whitespace, with the separator whitespace retained on the preceding fragment. Never split inside backtick runs: a run opens inline code and only an equal-length run closes it; an unclosed run protects the remainder. A period is not a boundary when its preceding whitespace-delimited token contains `/`, `\\`, `::`, a digit, another period, or is one of the case-insensitive fixed abbreviations `mr. mrs. ms. dr. prof. sr. jr. st. vs. etc. e.g. i.e.`. This conservatively protects path/version/decimal dots, initials with multiple periods, and abbreviations. A sentence fragment still over either size limit splits at semicolons followed by whitespace outside inline backticks. Every resulting fragment retains exact byte bounds; no fragments are silently truncated. An oversized fragment without a valid boundary remains oversized and skipped. Fenced blocks never split. Adjacent sentence/clause fragments may form the same bounded windows as other units. The earlier no-sentence-splitting statement is superseded only for oversized nonfence prose/list units.

Blank lines between adjacent units are retained in window unions. Each unit has a section number and ordinal. Register every nonempty base unit that has at most 160 content tokens and 2,048 UTF-8 bytes. Register at most one preferred window per unit: its exact union with the previous and next unit in the same section, with at most 3 units total, provided the union has at most 160 content tokens and 2,048 bytes. If the 3-unit union fails, prefer the unit plus next neighbor; then previous neighbor plus unit; then the unit alone. No window crosses sections or an oversized unit. Keep oversized-unit/fence counts, skipped bytes, skipped-note counts, representable original-body byte fraction, and notes with any/full registered representation. Blank-only byte gaps excluded from base units are counted explicitly so representability cannot hide loss in its denominator.

A derived Source keeps the exact original source ID, body, scope, revision, title and clocks, but contains registered passage Facts. Each Fact ID is `p` plus the first 16 hex characters of a digest of policy ID, original body SHA256, revision and byte bounds. Detect collisions explicitly. Keep original source fingerprints and SHA256 of original UTF-8 body bytes separately. Derived Source fingerprints are expected to differ because facts differ; never compare them as original fingerprints. Derived Fact subject and validity fields follow the source's original drawer facts, predicate is `memory_text`, claim/object is the exact slice, and standing is false. This experiment rejects heterogeneous drawer fact clocks or partially ineligible drawer facts because whole-body expansion would otherwise bypass eligibility; the current adapter emits homogeneous source clocks.

For each selected seed in order, every intersecting base unit is eligible for allocation, in source byte order. Only units intersecting the seed are considered centers. Its preferred window is the first choice; the registered base unit is the smaller fallback. The allocator may admit only a whole registered choice. Any overlap with already emitted bytes rejects that choice; try its base unit instead, then report overlap if that too overlaps. Canonical complete-packet token count determines fit. A rejected window may fall back to its base unit for budget. No byte truncation, partial fence, fabricated quote, arbitrary gap join, or query-time unregistered merge is permitted. Empty/oversized units receive explicit diagnostics. Source-local unit order within each seed is stable; global seed order is native rank.

`expanded_candidates` is the union of all registered preferred windows and base alternatives reachable from selected seed units before budget allocation. It is a distinct potential coverage diagnostic, not native candidate coverage. `selected_seed_spans` remains the original fixed-chunk intervals. Every emitted passage records the seed IDs whose intervals it intersects. Expansion bytes outside the selected seed union are reported explicitly. Atomic fences are preserved by the passage arm; fixed controls retain their original fixed-chunk behavior.

## Typed models and public signatures

The following signatures are contracts only. Imported legacy types retain their existing definitions. JSON crossing a file/helper boundary uses existing validated `JSON` helpers; internal records are fully typed dataclasses. No `Any` or new runtime validation dependency is required.

```python
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, Mapping, Sequence
import tiktoken
from legacy import Evidence, Source, Packet, RustHelper, JSON
from contracts import Task, Selection
from plan_index import ScopedIndex

Arm = Literal['raw_bm25', 'lexical_fixed', 'lexical_passages']
Status = Literal['positive', 'negative', 'unavailable', 'ambiguous']

@dataclass(frozen=True)
class Span:
    source_id: str
    start_byte: int
    end_byte: int
    source_body_sha256: str

@dataclass(frozen=True)
class Unit:
    span: Span
    section: int
    ordinal: int
    atomic: bool
    oversized: bool

@dataclass(frozen=True)
class PassageView:
    task_context: tuple[str, str, str]
    sources: tuple[Source, ...]
    units: Mapping[str, tuple[Unit, ...]]
    base: Mapping[Span, Evidence]
    preferred: Mapping[Span, Evidence]
    original_fingerprints: Mapping[str, str]
    body_digests: Mapping[str, str]
    representation: Mapping[str, JSON]

@dataclass(frozen=True)
class Lexicon:
    task_context: tuple[str, str, str]
    source_count: int
    document_frequency: Mapping[str, int]
    drawer_bodies: Mapping[str, str]

@dataclass(frozen=True)
class PackedPassages:
    packet: Packet
    expanded_candidates: tuple[Evidence, ...]
    seed_ids: Mapping[str, tuple[str, ...]]
    rejections: tuple[tuple[str, str], ...]

@dataclass(frozen=True)
class Judgment:
    query_id: str
    status: Status
    supporting_spans: tuple[Span, ...]

def build_lexicon(sources: tuple[Source, ...],
                  metadata: Mapping[str, Mapping[str, JSON]], task: Task) -> Lexicon: ...
def select_lexical(task: Task, candidates: tuple[Evidence, ...],
                   lexicon: Lexicon) -> Selection: ...
def derive_passages(sources: tuple[Source, ...],
                    metadata: Mapping[str, Mapping[str, JSON]], task: Task,
                    encoding: tiktoken.Encoding) -> PassageView: ...
def pack_passages(task: Task, selection: Selection, view: PassageView,
                  budget: int, helper: RustHelper,
                  encoding: tiktoken.Encoding) -> PackedPassages: ...
def validate_passages(result: PackedPassages, originals: tuple[Source, ...],
                      view: PassageView, selection: Selection, task: Task,
                      budget: int, helper: RustHelper,
                      encoding: tiktoken.Encoding) -> None: ...

def validate_gold(data: Mapping[str, JSON], prepared: Mapping[str, JSON]
                  ) -> tuple[Judgment, ...]: ...
def score_spans(returned: Sequence[Span], required: Sequence[Span]
                ) -> dict[str, JSON]: ...
def run_case(task: Task, arm: Arm, index: ScopedIndex, helper: RustHelper,
              budget: int, encoding: tiktoken.Encoding,
              originals: tuple[Source, ...], lexicon: Lexicon,
              view: PassageView) -> dict[str, JSON]: ...
def evaluate(prepared: Path, gold: Path, output: Path, helper_path: Path,
              cache: Path, expected_prepared_sha256: str,
              expected_gold_sha256: str) -> None: ...
def main(argv: Sequence[str] | None = None) -> int: ...
```

Policy constants, token/anchor extraction, boundary scanning, and JSON serialization helpers are private. No public class contains behavior. No new exception hierarchy is needed: existing `ExperimentError` contains `FixtureError` for malformed input, `ProtocolError` for helper failure, and `IntegrityError` for violated source/context/budget/determinism contracts. OS/subprocess failures propagate with a nonzero CLI exit; no silent fallback. No imports perform work beyond existing sibling module loading.

`build_lexicon` and `derive_passages` require validated unique source identities and metadata, same scope/cutoff/as-of context, and the adapter's homogeneous drawer validity. Their outputs derive only from eligible original notes. Graph rows never enter these outputs. `select_lexical` requires at most 20 distinct authoritative fixed candidates in native order; output is an ordered subset of at most 8 and includes explicit rejection reasons. The caller validates task/index context before search, then checks selected seeds against that authoritative index.

`pack_passages` requires that all selected seeds resolve to the original drawer bodies in the view and the task context remains eligible. It returns only registered exact-source passages and a complete canonical packet within budget. `validate_passages` first verifies original body hashes, original fingerprints, seed authority, view metadata, registered byte slices, expansion reachability and nonoverlap, then invokes the unchanged `validate_packet` with derived authoritative Sources. A caller cannot make an arbitrary fabricated view authoritative merely by supplying matching packet sidecars; registration is independently reconstructed from originals and the frozen policy during validation outside timing.

`run_case` receives no query ID, label, logging time, category, required spans or acceptable IDs. It returns candidates, selected seed IDs, expanded candidate IDs, emitted evidence spans/IDs, exact text/tokens, source kinds, reasons and timing samples. Warmup plus 3 measured repetitions must produce identical signatures with timing excluded. Source and packet validation is outside measured intervals. Stage timings distinguish native retrieval, lexical selection and packing; total query latency includes all three. Setup timings separately cover native indexing, lexicon and passage registration.

`evaluate` is the only content-bearing file-writing boundary. Prepared and gold hashes must match before measurement. Private output uses existing exclusive mode-0600 writers in a new mode-0700 directory outside Git. Native scratch must resolve under an explicitly provided private TMPDIR outside Git. Frozen manifest contains policy, new/reused code, helper, encoding, prepared/snapshot/sample/gold hashes and the exact task context. No content is printed; the CLI returns artifact paths, hashes and aggregate status only.

## Original-span gold conversion and metrics

The independent judge continues the existing schema with `query_id`, `status`, and `supporting_spans`. The new evaluator converts each supporting span into `Span` by resolving original source bytes and recording its body SHA256. If a judge supplies a body digest or quote, both must match those authoritative bytes. Prepared snapshot hash plus original source bytes pin otherwise digest-less annotations. Legacy `required`, `acceptable`, `forbidden`, `corroboration` and optional alternative annotations remain private annotation data and do not determine primary support. No chunk IDs are used to infer required support.

Every `supporting_spans` entry is required for primary complete support. This supports multi-span conjunctions directly. Alternatives must not be inserted into that array. Optional alternatives are annotations only and receive no primary metric credit unless a separately approved complete alternative contract replaces this one before measurement. The current experiment has no alternative-bundle scoring.

Gold validation requires all 32 unique sample IDs exactly once; valid status; original eligible drawer source; bounds and UTF-8 boundaries; matching optional digest/quote; at least one required span for positives, and no required spans for negative/unavailable/ambiguous. Duplicate identical spans are invalid. Overlapping distinct spans are valid. Conversion does not rewrite the independently frozen gold file.

For each positive and each stage (native candidate, selected seed, expanded candidate, emitted), `score_spans` returns:

- `byte_coverage`: bytes in the intersection of returned and required unions divided by required union bytes; union per source first.
- `full_span_fraction`: number of individually required spans wholly covered by the returned interval union divided by required span count.
- `complete`: all required spans wholly covered. Any gap fails completeness; one-byte overlap is insufficient.
- `note_recall`: required source IDs with at least one supported byte returned divided by required source IDs.

Selected and emitted graph rows never count toward note support, but kind counts report their budget/candidate presence in the control. Expanded coverage can exceed native or selected coverage and is labeled accordingly. Empty positives score zero, including `complete=false`. Scores aggregate as macro averages over positives, reporting the denominator for every metric. All required spans rather than alternate annotations define byte denominators. An empty required set is not vacuously complete: `score_spans` returns null support measures and is excluded from positive means.

Negative abstention is empty packets divided by negative queries only. Selector abstention, positive empty packets, budget-only empty packets, unavailable count and ambiguous count are separate. Unannotated emitted text is unresolved relevance. Report paired fixed-versus-passage changes at both budgets, not only favorable outcomes. Source-created-before/after prompt strata use preserved original source metadata, explicitly excluding unknown creation dates and without equating creation with verification. Primary support does not depend on fixed chunk IDs, passage counts, or group IDs.

## Synthetic acceptance and verification contract

Tests use invented sources/prompts only. Cover native candidate identity across arms; identical lexical seeds at both budgets; identifier-only, ordinary two-term, generic-only, zero-match and trailing-anchor prompts; rarity and overlap boundary cases; KG exclusion and stable native ties; UTF-8/emoji, CRLF, blank separators, path/version punctuation, list continuations, heading boundaries, closed/unclosed and oversized fences; whole-unit budget limits including native formatting overhead; overlapping windows and fallback; exact spans across overlapping returned ranges versus a one-byte gap; overlapping gold; zero positives; nonpositive exclusions; original digest/revision changes, missing source, spoofed facts/offsets/text and duplicate evidence; scope/time mismatch, exact expiry boundary, deleted/superseded and partly ineligible drawers; private output exclusivity and frozen hashes.

Before official measurement: focused full new-experiment pytest suite, strict mypy on new source modules, frozen-helper synthetic integration/CLI smoke, existing real-validation regression tests, line cap, independent critic/security review, independent gold/input validation. Tests and output go to task-specific private scratch files with explicit exit status. Parent owns the review dispatches and official run authorization. Stage 2 itself only creates this document; no tests or code execution claims are made.
