# Design: Per-File Map-Reduce Review for trusty-review

**Backlog item**: per-file map-reduce review (size L, needs spec)
**Date**: 2026-06-03
**Status**: DRAFT — awaiting maintainer sign-off on the "Open Questions" section
**Crate**: `trusty-review` (`crates/trusty-review/`)
**Author**: Research analysis + design draft

---

## Summary

Today every review sends **one unified, noise-filtered diff** to a **single
reviewer LLM call**, capped at `MAX_DIFF_CHARS` (160 000 chars ≈ 40 K tokens).
On large PRs the diff is truncated at a hunk boundary and the tail is silently
dropped — the reviewer literally never sees the omitted files. Verdict quality
degrades, and the truncation marker is the only signal.

This spec designs a **map-reduce review mode**: instead of one large pass, we
**map** each changed file to an independent per-file review (in bounded
parallel), then **reduce** the per-file verdicts and findings into one
synthesized verdict (`APPROVE` / `APPROVE*` / `REQUEST_CHANGES` / `BLOCK` /
`UNKNOWN`) with a deduped, prioritized findings list. The mode is **additive**:
the existing unified path is untouched; a heuristic (or explicit flag/env)
selects between them.

The design deliberately **reuses** three existing patterns already in the crate:
- the `DiffAnalyzer` Stage-A/B/C noise filter and its per-file `FilteredFile`
  model (`pipeline/diff_analyzer/`),
- the bounded-concurrency `buffer_unordered` fan-out from the verification round
  (`pipeline/verify.rs:147`),
- the deterministic Jaccard dedup + LLM-narrative synthesis pattern from the
  contributor-profile `Synthesizer` (`profile/synthesizer.rs`).

---

## Part 1 — Investigation: the current review pipeline

All citations are `path:line` into `crates/trusty-review/src/` at worktree base
commit `8127d63`.

### 1.1 The single entry point: `run_review`

`pipeline/runner.rs:147` `run_review(config, input, deps) -> ReviewResult` is the
sole pipeline entry. The CLI `run`/`compare` and all three MCP tools route
through it:
- CLI `cmd_run` → `run_review` at `commands/run.rs:102`.
- MCP `review_pr` → `run_review` at `mcp/tools.rs:209`; `review_diff` →
  `run_review` at `mcp/tools.rs:260` (writes the diff to a temp file and uses
  `DiffSource::LocalFile`).
- `ReviewInput` (`runner.rs:76`) carries `diff_source`, `reviewer_model`,
  `write_log`, `print_result`, `trigger`, `run_mode`, `allow_posting`.
- `ReviewDeps` (`runner.rs:111`) carries `llm`, optional `verifier`, `search`,
  optional `analyze`, optional `dedup` — all trait objects behind `Arc`.

The runner's ordered steps (`runner.rs:152`–`384`):
1. derive owner/repo/pr from the `DiffSource` (`runner.rs:153`),
2. fetch PR metadata + head SHA (`runner.rs:170`),
3. dedup claim keyed on `(owner,repo,pr,head_sha)` (`runner.rs:204`),
4. **load → filter → truncate diff** (`runner.rs:231`–`244`),
5. extract identifiers + changed files for context (`runner.rs:247`–`248`),
6. required-context gate (`runner.rs:256`, `preflight_context`),
7. gather context in parallel (`runner.rs:281`, `tokio::join!`),
8. build prompt + single LLM call (`runner.rs:296`–`315`),
9. parse verdict + findings (`runner.rs:340`),
10. severity-anchored floor (`runner.rs:357`, `derive_verdict`),
11. per-finding verification round (`runner.rs:374`, `maybe_verify`),
12. finalize: post-or-log (`runner.rs:384`, `finalize_run`).

### 1.2 DiffAnalyzer Stage A/B/C + `MAX_DIFF_CHARS` (the degradation point)

The diff is filtered then truncated at `runner.rs:241`–`243`:

```rust
let filtered = DiffAnalyzer::default().analyze(&raw_diff).await;
let max = crate::config::constants::MAX_DIFF_CHARS;
let diff = truncate_diff(&filtered.render_for_prompt(max));
```

- `DiffAnalyzer::analyze` (`diff_analyzer/mod.rs:84`) runs:
  - **Stage A** — `FileFilter` (`diff_analyzer/file_filter.rs`): drops lockfiles /
    snapshots / generated files entirely, or collapses fixtures to a
    `SummaryOnly` line. Returns `(kept_files, dropped_files)` (`mod.rs:93`).
  - **Stage B** — `HunkFilter` (`diff_analyzer/hunk_filter.rs`): deterministically
    drops whitespace-only / import-only / comment-only hunks (`mod.rs:101`).
  - **Stage C** — optional Haiku `HunkClassifier` (`diff_analyzer/hunk_classifier.rs`),
    **disabled by default** (`FilterConfig::disable_classifier`, `mod.rs:107`).
- The result is a `FilteredDiff` (`diff_analyzer/models.rs:177`) whose
  `files: Vec<FilteredFile>` (`models.rs:134`) is **already per-file**, each
  carrying `filename`, `status` (`"added"`/`"modified"`/`"renamed"`/`"removed"`),
  `disposition` (`Kept`/`SummaryOnly`), `hunks: Vec<FilteredHunk>`,
  `dropped_hunks`, and `summary_line`. **This is the natural map unit.**
- `FilteredDiff::render_for_prompt(max_chars)` (`models.rs:200`) renders kept
  files+hunks into a single string, **stopping with `break` once `max_chars`
  is reached** (`models.rs:209`, `:218`, `:225`) — i.e. **later files are
  silently dropped** when the budget is exhausted.
- `truncate_diff` (`diff/diff.rs:92`) is the final safety net: if still over
  `MAX_DIFF_CHARS` it cuts at the last `\n@@` hunk boundary
  (`diff/diff.rs:99`) and appends `"[DIFF TRUNCATED — N chars omitted; review
  may be incomplete]"` (`diff/diff.rs:110`).
- `MAX_DIFF_CHARS = 160_000` (`config/constants.rs:111`), raised from 60 K in
  #624/#627 specifically to give the noise filter headroom — but it is still a
  hard ceiling, and a single 200 K-char generated-but-not-filtered file or a
  many-file PR overruns it.

**Degradation summary**: on a big PR, `render_for_prompt` + `truncate_diff`
together guarantee the reviewer sees only the first ~160 K chars; everything
after a `break`/cut is invisible. The verdict is formed on a partial diff with
no per-file accounting of what was lost.

### 1.3 Verdict + findings production (the LLM call, schema, parse, floor, verify)

- **Prompt** — `build_review_prompt` (`pipeline/prompt.rs:211`) assembles an
  `LlmRequest` with `reviewer_system_prompt()` (`prompt.rs:139`, the
  5-grade rubric + compile-break rule), the diff block, and context blocks
  (`build_user_message`, `prompt.rs:284`). It attaches
  `review_response_schema()` (`prompt.rs:58`) for forced structured output —
  `verdict`, `summary`, `findings[]` with `{title, body, severity, confidence,
  file, line}`. Reviewer temperature `0.3`, max tokens `4096` (`prompt.rs:37`,
  `:40`).
- **LLM trait** — `LlmProvider::complete(LlmRequest) -> Result<LlmResponse,
  LlmError>` (`llm/mod.rs:155`). `LlmResponse` (`llm/mod.rs:112`) carries
  `text`, `model`, `input_tokens`, `output_tokens`, `latency_ms`, `cost_usd`.
- **Parser** — `parse_review_response(&text) -> ParsedReview`
  (`pipeline/parser.rs:133`): direct-JSON → fenced-JSON → keyword-scan →
  fail-safe `APPROVE` (`parser.rs:141`–`172`). `ParsedReview` (`parser.rs:84`)
  has `verdict`, `summary`, `findings: Vec<Finding>`, `is_fail_safe`. Findings
  map severity→`Effort` (`parser.rs:260`: high/critical→High, medium→Medium,
  else Low).
- **Models** — `Verdict` (`models/mod.rs:43`: Approve / ApproveWithReservations
  (`APPROVE*`) / RequestChanges / Block / Unknown); `Finding` (`models/mod.rs:156`:
  `file`, `line`, `kind`, `description`, `suggestion`, `confidence` clamped
  `[0,1]`, `effort`, transient `verified: Option<VerifyOutcome>`,
  `issue_eligible`); `ReviewResult` (`models/mod.rs:222`).
- **Severity floor** — `derive_verdict(model_proposed, findings)`
  (`pipeline/grade.rs:92`): low-confidence override (`grade.rs:104`, all
  findings ≤0.65 conf & no High → `APPROVE`) then `severity_floor`
  (`grade.rs:149`: any High→`BLOCK`; ≥2 Medium→`REQUEST_CHANGES`; 1
  Medium→`APPROVE*`; else `APPROVE`) combined via `stricter_of`
  (`grade.rs:192`). `Unknown` is always preserved.
- **Verifier** — `maybe_verify` (`pipeline/verify.rs:70`) →
  `run_verification_round` (`verify.rs:115`): selects candidates by verdict
  (`select_candidates`, `verify.rs:260`), verifies each concurrently with
  `buffer_unordered(VERIFY_CONCURRENCY=4)` (`verify.rs:53`, `:161`), demotes
  refuted findings to conf `0.10` (`verify.rs:352`, `VERIFY_REFUTED_CONFIDENCE`),
  and **re-derives** the verdict from survivors (`verify.rs:217`).

### 1.4 Context builders (what feeds the prompt)

- `gather_context` (`pipeline/runner_context.rs:42`) runs three sources
  concurrently (`runner_context.rs:130`, `tokio::join!`): trusty-search code
  search (query = identifiers + PR title, capped 5 terms, `runner_context.rs:51`),
  trusty-analyze complexity/smells filtered to changed files
  (`runner_context.rs:96`,`:107`), and **APEX/KB** via `fetch_apex_context`
  (`integrations/apex_context.rs`, gated by `config.apex_index`,
  `runner_context.rs:119`). Returns `ReviewContext` (`prompt.rs:108`).
- `gather_external_context_md` (`runner_context.rs:189`) renders JIRA /
  Confluence / **GitHub Issues** markdown via the context orchestrator
  (`integrations/context/`). All sources are **fail-open**.
- Both run on the **whole PR** today — the search query and analyze probe are
  PR-level, not per-file.

### 1.5 Token/cost accounting + Bedrock+verifier pipeline

- Per-call telemetry is on `LlmResponse` (`llm/mod.rs:118`–`124`); the runner
  folds it onto `ReviewResult` via `apply_llm_response` (`models/mod.rs:327`:
  `input_tokens`, `output_tokens`, `cost_estimate_usd`, `latency_ms`). There is
  **no cross-call accumulator** in the review pipeline today (the contributor
  profile has one — `TokenCost::accumulate`, used at `profile/synthesizer.rs:115`
  — which we will mirror).
- Providers are built by `build_provider` (`llm/mod.rs:224`) with `bedrock/` /
  `openrouter/` prefix routing (`resolve_provider_and_model`, `llm/mod.rs:202`).
  Per-role models resolve through `RoleModels` (`config/role_models.rs:74`:
  reviewer 0.3/4096, verifier 1.0/16, summarizer 0.0/4096) over a 4-level
  precedence chain (CLI → `TRUSTY_REVIEW_*` env → TOML → default).

### 1.6 Reusable prior art

- **Bounded fan-out**: `verify.rs:147` `stream::iter(...).map(...)
  .buffer_unordered(N).collect().await` — exact shape we need for the map stage.
- **Dedup + synthesis**: `profile/synthesizer.rs:160` `assign_trend_tags` +
  `jaccard_similarity` (`synthesizer.rs:253`, threshold 0.7) + one LLM narrative
  call with deterministic fallback (`synthesizer.rs:88`). The reduce stage is a
  direct analogue.
- **Existing dedup constant**: `FINDING_SIMILARITY_THRESHOLD = 0.60`
  (`config/constants.rs:91`) is already defined for related-finding dedup.

---

## Part 2 — The map-reduce design

### 2.0 Shape chosen

> **Deterministic per-file split → bounded-parallel per-file LLM reviews (map)
> → deterministic dedup + one LLM synthesis pass (reduce) → existing
> floor + verify on the merged finding set.**

The map stage produces *N* `ParsedReview`-equivalents (one per file group). The
reduce stage merges them into a single `(verdict, Vec<Finding>, summary)` that
re-enters the **existing** `derive_verdict` + `maybe_verify` machinery unchanged
(`runner.rs:357`,`:374`). This keeps the calibration work (#583, grade floor)
fully intact and limits map-reduce to the diff-handling + aggregation seam.

### 2.1 Map stage — splitting the diff

**Unit of map = a `FilteredFile`** (post Stage-A/B). The `DiffAnalyzer` already
produces `FilteredDiff.files: Vec<FilteredFile>` (`diff_analyzer/models.rs:179`),
so the split is *free* — we iterate `files` instead of calling
`render_for_prompt` once. New code lives in a `pipeline/mapreduce/` module
(not in `runner.rs`, to respect the 500-line cap and keep the runner thin).

**Per-file char budget + sub-chunking huge files.** Each map unit gets a budget
`TRUSTY_REVIEW_MAP_PER_FILE_CHARS` (default `120_000`). A new
`FilteredFile::render_one(max_chars)` (mirroring `models.rs:200` but for a single
file) renders that file's surviving hunks. If a single file's rendered hunks
exceed the budget, we **sub-chunk by hunk**: pack whole `FilteredHunk`s
(`models.rs:79`, never split a hunk mid-body) greedily into multiple
`MapUnit`s for the *same* file, each tagged `chunk_index/chunk_total`. A hunk
larger than the budget on its own is emitted as its own unit and char-truncated
with a per-unit marker (same wording as `diff/diff.rs:110`). Sub-chunks of one
file are reduced together (their findings carry the same `file`).

**Renames / binary / deleted / summary-only files.**
- **Deleted** (`status == "removed"`): no surviving `+` content; emit a
  **metadata-only** `MapUnit` (no LLM call) noting the deletion so the reduce
  stage can still flag "callers of a deleted file" — but defer the
  cross-file-reference check to §2.2 (it needs the whole file list). Skipping
  the LLM call here is the single biggest cost saver on refactor PRs.
- **Renamed** (`status == "renamed"`): if Stage-A/B left no substantive hunks,
  treat as metadata-only (pure rename); otherwise review the surviving hunks
  normally with a `renamed from … ` note in the per-file prompt.
- **Binary**: the unified-diff parser (`diff_analyzer/mod.rs:174`
  `parse_diff_files`) yields no `@@` hunks for binary files, so they naturally
  produce empty `MapUnit`s → metadata-only, no LLM call.
- **`SummaryOnly`** (`FileDisposition::SummaryOnly`, `models.rs:29`): keep the
  collapsed one-liner as metadata-only context for reduce; never spend an LLM
  call on a fixture.

**Per-file review prompt shape.** A new `build_file_review_prompt(map_unit,
file_context, model)` mirrors `build_review_prompt` (`prompt.rs:211`) but:
- system prompt is the **same** `reviewer_system_prompt()` (`prompt.rs:139`) with
  one appended paragraph: *"You are reviewing ONE FILE of a larger PR. Judge only
  this file; cross-file concerns will be reconciled separately. Set `file` on
  every finding to this path."*
- user message contains the PR header, the **single file's** diff, and that
  file's scoped context (see below).
- the **same** `review_response_schema()` (`prompt.rs:58`) is reused verbatim so
  the existing `parse_review_response` (`parser.rs:133`) handles each map
  response with zero changes.

**Per-file context (scoped vs shared).**
- **Shared once** (computed before the fan-out, cloned by ref into each unit):
  PR title/body, external context markdown (`gather_external_context_md`,
  `runner_context.rs:189`) — these are PR-level by nature and re-querying per
  file would multiply latency/cost for no signal gain.
- **Scoped per file** (cheap to slice from already-gathered PR context):
  trusty-analyze `complexity_hotspots` / `smells` are already filtered to
  changed files (`runner_context.rs:96`,`:107`); we additionally filter each to
  `unit.file`. trusty-search results and APEX results are matched to the file by
  path/identifier; an empty slice is fine (the prompt blocks are all
  `if !empty`, `prompt.rs:318`+). **Default: do NOT issue a new per-file search
  query** — reuse the PR-level `ReviewContext` and slice it. A
  `TRUSTY_REVIEW_MAP_PER_FILE_SEARCH=1` opt-in (off by default) can enable a
  focused per-file search for highest fidelity at higher cost (Open Question 2).

### 2.2 Reduce stage — aggregating into one verdict

Input: `Vec<MapOutcome>` where `MapOutcome { file, chunk_index, parsed:
ParsedReview, telemetry }`. Reduce runs in three deterministic-then-LLM steps,
mirroring `synthesizer.rs:88`.

**Step R1 — collect + per-file verdict reconciliation.** Group `MapOutcome`s by
`file`; a file with sub-chunks takes the **stricter** of its chunk verdicts via
the existing `stricter_of` ordering (re-exposed from `grade.rs:192`). Concatenate
each file's findings. Metadata-only units contribute no verdict.

**Step R2 — finding dedup + prioritization (deterministic).** Flatten all
findings. Dedup with the **existing** Jaccard helper (`jaccard_similarity`,
`synthesizer.rs:253`) at `FINDING_SIMILARITY_THRESHOLD = 0.60`
(`config/constants.rs:91`) — but **only collapse findings on the same `file`+
nearby `line`** (cross-file findings are kept distinct). When collapsing, keep
the **highest-confidence / highest-`Effort`** representative and union the
suggestions. Then **prioritize**: sort by (`Effort` desc, `confidence` desc) and
cap the surfaced list at `TRUSTY_REVIEW_MAP_MAX_FINDINGS` (default `50`) so a
huge PR cannot produce an unbounded comment; overflow is summarized as a count.

**Step R3 — verdict precedence + LLM synthesis.**
- **Precedence (deterministic, computed first as a floor):**
  `BLOCK > REQUEST_CHANGES > APPROVE* > APPROVE`, with `UNKNOWN` handled
  specially. Any per-file `BLOCK` ⇒ overall floor `BLOCK`; any
  `REQUEST_CHANGES` ⇒ floor `REQUEST_CHANGES`; etc. `UNKNOWN` per-file does **not**
  poison the whole verdict — it means "this file was unassessable"; we record an
  `unassessed_files` count and only emit overall `UNKNOWN` when **every**
  non-metadata file came back `UNKNOWN` (or failed — see §2.6).
- **Synthesis LLM pass (one call, optional):** a `synthesizer`-role call
  (reuse the `summarizer` role config, `role_models.rs:118`, temp 0.0) receives
  the deduped finding list + per-file verdicts and returns a one-paragraph
  cross-file `summary` and may surface **cross-file findings** the per-file pass
  could not see (e.g. "file A deletes `enum V`, file B still matches on it" —
  the compile-break rule at `prompt.rs:162`). It returns the **same**
  `review_response_schema` so parsing is unchanged. The synthesis pass **does
  re-read** a compact cross-file view: the list of changed files + each file's
  surviving-symbol deltas (cheap, derived from `extract_identifiers`,
  `diff/diff.rs:130`), NOT the full diffs.
- The final merged verdict = `stricter_of(deterministic_precedence_floor,
  synthesis_verdict)`. This merged `(verdict, findings)` then flows into the
  **existing** `derive_verdict` (`runner.rs:357`) and `maybe_verify`
  (`runner.rs:374`) untouched — so the grade floor and verification round apply
  to the merged set exactly as today.
- **Fail-safe:** if the synthesis call fails, we keep the deterministic
  precedence floor + deduped findings and a templated summary (mirror
  `apply_fallback_narrative`, `synthesizer.rs:398`). Reduce never fails the
  review.

### 2.3 Concurrency & budget

- **Parallel map calls** via the verify-round pattern (`verify.rs:147`):
  `stream::iter(units).map(|u| review_one_file(...)).buffer_unordered(MAP_CONCURRENCY)`.
  `TRUSTY_REVIEW_MAP_CONCURRENCY` default `4` (matches `VERIFY_CONCURRENCY`,
  `verify.rs:53`) — caps provider rate-limit/burst.
- **Total token/cost budget.** `TRUSTY_REVIEW_MAP_TOTAL_CHAR_BUDGET` (default
  `1_000_000` chars ≈ 250 K tokens of *input* across all map calls). The
  splitter packs files until the cumulative budget is hit; remaining files are
  **batched** (multiple small files concatenated into one `MapUnit` up to the
  per-file budget) rather than dropped — so coverage degrades to "fewer, larger
  calls" instead of "silent truncation." A hard map-call ceiling
  `TRUSTY_REVIEW_MAP_MAX_CALLS` (default `40`) bounds worst-case cost; beyond it,
  remaining files are grouped into the last few calls.
- **Cost accounting.** A new `MapReduceTelemetry { map_calls, reduce_calls,
  input_tokens, output_tokens, cost_usd, latency_ms }` accumulates each
  `LlmResponse` (mirror `TokenCost::accumulate`, `synthesizer.rs:115`) and is
  folded onto `ReviewResult` (`input_tokens`/`output_tokens`/
  `cost_estimate_usd`/`latency_ms` become **sums**; `latency_ms` is wall-clock of
  the whole fan-out, not the sum). A new optional `ReviewResult.mapreduce:
  Option<MapReduceStats>` field (serde `skip_serializing_if = "Option::is_none"`,
  backward-compatible) records `files_reviewed`, `files_metadata_only`,
  `files_failed`, `unassessed_files`, `map_calls`, `reduce_calls`.
- **A single file still over budget.** Handled in §2.1 sub-chunking: split by
  hunk; a lone over-budget hunk is char-truncated with a per-unit marker. The
  reduce stage records this in `MapReduceStats` so the verdict is honestly
  labelled partial (analogous to the existing truncation marker).

### 2.4 When to use it (unified vs map-reduce)

`select_review_mode(filtered: &FilteredDiff, config) -> ReviewMode` returns
`Unified` or `MapReduce`:
- **Automatic trigger (default ON):** choose `MapReduce` when the **rendered
  unified diff would truncate** — i.e. `filtered.render_for_prompt(MAX_DIFF_CHARS)`
  would exceed the cap **OR** `filtered.files.len() >
  TRUSTY_REVIEW_MAP_FILE_THRESHOLD` (default `12`). The first condition is the
  precise "we are about to lose content" signal; the second catches the
  many-small-files case where per-file isolation improves precision even under
  the cap.
- **Override knob** `TRUSTY_REVIEW_MAP_MODE` ∈ `{auto, always, never}` (default
  `auto`). `never` preserves today's behaviour exactly; `always` forces
  map-reduce (useful for A/B and `compare`).
- The decision is logged at `info` (stderr) with the deciding metric so
  operators can see why a PR took which path.

This is **automatic-by-default with an explicit escape hatch** — large PRs (the
ones that degrade today) silently get the better path; small PRs keep the cheaper
single call.

### 2.5 Types & API surface

New module `pipeline/mapreduce/` (thin `mod.rs` + focused submodules, each
< 500 lines per the cap):

```rust
// pipeline/mapreduce/mod.rs — re-export facade
pub use split::{MapUnit, MapUnitKind, split_into_units};
pub use map::{MapOutcome, run_map_stage};
pub use reduce::{ReducedReview, run_reduce_stage};
pub use mode::{ReviewMode, MapReduceConfig, select_review_mode};

/// One reviewable slice of the PR (a whole file, a file sub-chunk, or metadata).
pub struct MapUnit {
    pub file: String,
    pub status: String,                 // added/modified/renamed/removed
    pub kind: MapUnitKind,              // Review { diff_text } | MetadataOnly { note }
    pub chunk_index: usize,            // 0-based; 0/1 for non-chunked
    pub chunk_total: usize,
}
pub enum MapUnitKind { Review { diff_text: String }, MetadataOnly { note: String } }

/// Result of reviewing one MapUnit (or a no-op for metadata units).
pub struct MapOutcome {
    pub file: String,
    pub chunk_index: usize,
    pub parsed: Option<ParsedReview>,  // None for metadata-only / failed-dropped
    pub failed: bool,
    pub telemetry: Option<LlmResponse>,
}

/// Deterministic-plus-synthesis reduce output handed back to the runner.
pub struct ReducedReview {
    pub verdict: Verdict,              // precedence floor (pre derive_verdict)
    pub summary: String,
    pub findings: Vec<Finding>,        // deduped + prioritized + capped
    pub stats: MapReduceStats,
}
```

Config additions (`config/mapreduce.rs`, resolved like `VerificationConfig`,
`config/verification.rs`, and surfaced on `ReviewConfig`, `config/mod.rs:96`):

| Field | Env var | Default | Purpose |
|---|---|---|---|
| `mode` | `TRUSTY_REVIEW_MAP_MODE` | `auto` | `auto`/`always`/`never` |
| `file_threshold` | `TRUSTY_REVIEW_MAP_FILE_THRESHOLD` | `12` | auto-trigger file count |
| `per_file_chars` | `TRUSTY_REVIEW_MAP_PER_FILE_CHARS` | `120000` | per-unit char budget |
| `concurrency` | `TRUSTY_REVIEW_MAP_CONCURRENCY` | `4` | parallel map calls |
| `total_char_budget` | `TRUSTY_REVIEW_MAP_TOTAL_CHAR_BUDGET` | `1000000` | sum of map inputs |
| `max_calls` | `TRUSTY_REVIEW_MAP_MAX_CALLS` | `40` | hard map-call ceiling |
| `max_findings` | `TRUSTY_REVIEW_MAP_MAX_FINDINGS` | `50` | surfaced-finding cap |
| `synthesis_enabled` | `TRUSTY_REVIEW_MAP_SYNTHESIS` | `true` | enable reduce LLM pass |
| `per_file_search` | `TRUSTY_REVIEW_MAP_PER_FILE_SEARCH` | `false` | focused per-file search |

Conventions honoured: `thiserror` for any new lib error enum (no `anyhow` in
lib code); Why/What/Test doc triple on every public item; logs to **stderr**
only (the runner is also an MCP stdio server, `prompt`/`runner` already follow
this); no new public file > 500 lines (split `split`/`map`/`reduce`/`mode`).
The **runner change is minimal** (~20 lines): between current steps 7 and 12,
branch on `select_review_mode`; the unified path is the `else`. `derive_verdict`
(`runner.rs:357`) and `maybe_verify` (`runner.rs:374`) are called on the reduced
finding set identically.

### 2.6 Failure modes

- **One map call fails (transport / parse).** **Drop that file's review, do not
  fail the whole review.** Record `MapOutcome { failed: true }`, increment
  `MapReduceStats.files_failed`, and surface the file in the summary as
  "not reviewed (LLM error)". This mirrors the fail-open philosophy of context
  gathering (`runner_context.rs:75`) and the conservative-but-non-fatal verifier
  (`verify.rs:330`). Rationale: a partial review of 19/20 files is far more
  useful than a fail-safe blanket `APPROVE`.
- **All map calls fail.** Fall through to the existing fail-safe: verdict
  `APPROVE` with `error` set (matches the LLM-error path at `runner.rs:310`),
  `dry_run` true. We never post a verdict built on zero successful reviews.
- **Reduce/synthesis call fails.** Keep the deterministic precedence floor +
  deduped findings + templated summary (§2.2 R3 fail-safe). Non-fatal.
- **Partial-result reporting.** `MapReduceStats` (serialized on `ReviewResult`)
  always reports `files_reviewed` / `files_failed` / `files_metadata_only` /
  `unassessed_files`; the rendered review body prepends a one-line banner when
  any file was failed/metadata-truncated (reuse the `degraded_banner` style,
  `pipeline/context_gate.rs`), so a partial map-reduce review is never mistaken
  for a complete one — directly analogous to the existing `[DIFF TRUNCATED …]`
  honesty marker.
- **Dedup store interaction.** The dedup claim (`runner.rs:204`) is keyed on head
  SHA and is mode-agnostic; no change. Cost accounting sums across calls so a
  re-run is still suppressed correctly.

### 2.7 Worked example

PR touches 20 files, raw diff 380 K chars. Today: filtered to 240 K, then
`truncate_diff` cuts to 160 K → files 14–20 invisible; one Sonnet call;
verdict on 65 % of the diff.

Map-reduce (`auto` fires: render would exceed cap): Stage-A drops `Cargo.lock`
(1 file). 19 files → split: 17 fit one unit each, 1 large file (210 K) sub-chunks
into 2, 1 deleted file → metadata-only (no call). 19 review units, 18 LLM calls
at `buffer_unordered(4)`. Reduce: dedup 41 raw findings → 28; precedence floor =
`REQUEST_CHANGES` (two files flagged High-conf Medium bugs); synthesis pass
catches that file 7 deletes a method still called in file 12 → adds a `BLOCK`
compile-break finding (Effort::High). Merged → `derive_verdict` → `BLOCK`;
`maybe_verify` confirms the compile-break, refutes one speculative Medium →
final `BLOCK`. `MapReduceStats`: reviewed 18, metadata 1, failed 0, calls 19.

---

## Part 3 — Phased implementation plan

Each phase is one shippable PR, independently testable, behind the `never`
default until Phase 6 flips the default — so partial landings never change
production behaviour.

1. **PR-1 `feat(trusty-review): mapreduce config + mode selector (never default)`**
   Add `config/mapreduce.rs` (`MapReduceConfig`, all env knobs, resolved like
   `VerificationConfig`), surface on `ReviewConfig`, add `select_review_mode`
   returning `Unified` always until wired. Pure config + decision logic; unit
   tests for the auto-trigger thresholds. No runtime behaviour change.

2. **PR-2 `feat(trusty-review): per-file diff splitter (MapUnit)`**
   `pipeline/mapreduce/split.rs` + `FilteredFile::render_one`. `split_into_units`
   handles whole-file, hunk sub-chunking, rename/binary/deleted/summary-only,
   per-file + total-budget packing, batching. Heavily unit-tested against
   fixtures (reuse `diff_analyzer` test diffs). No LLM calls.

3. **PR-3 `feat(trusty-review): per-file map stage (bounded fan-out)`**
   `pipeline/mapreduce/map.rs` + `build_file_review_prompt`. `run_map_stage`
   fans out with `buffer_unordered`, drops failed units, returns `MapOutcome`s +
   telemetry. Tests inject a fake `LlmProvider` (as in `runner_tests.rs`).

4. **PR-4 `feat(trusty-review): reduce stage (dedup + precedence + synthesis)`**
   `pipeline/mapreduce/reduce.rs`. Deterministic R1/R2 + verdict precedence +
   optional synthesis pass (reuse Jaccard from `profile::synthesizer`; consider
   lifting `jaccard_similarity` to a shared `util` in a follow-up). Fail-safe
   fallback. Returns `ReducedReview`. Unit-tested without network.

5. **PR-5 `feat(trusty-review): wire map-reduce into run_review + MapReduceStats`**
   Branch in `runner.rs` on `select_review_mode`; add `ReviewResult.mapreduce`
   (backward-compatible optional field) + partial-result banner; cost
   accumulation. End-to-end runner tests for both paths with fakes. Still
   `never` default.

6. **PR-6 `feat(trusty-review): enable auto map-reduce by default + docs`**
   Flip `TRUSTY_REVIEW_MAP_MODE` default to `auto`; add a
   `regression-testing/` snapshot comparing unified vs map-reduce verdict/cost on
   a known large PR; update `crates/trusty-review/README.md` + this doc's status
   to ACCEPTED. (Optional follow-up PR-7: `compare` subcommand `--mode` flag for
   A/B; per-file focused search behind `TRUSTY_REVIEW_MAP_PER_FILE_SEARCH`.)

---

## Part 4 — Open questions / risks (human decision)

1. **Cost ceiling vs coverage on giant PRs.** Map-reduce can multiply LLM spend
   (up to `MAX_CALLS` calls + 1 synthesis vs 1 today). Is the default
   `max_calls=40` / `total_char_budget=1M` the right cost cap, or should there be
   a *dollar* budget (needs a pricing lookup the review pipeline doesn't
   currently own — only `LlmResponse.cost_usd` after the fact)? **Decision
   needed:** acceptable per-review cost multiplier.

2. **Per-file context: slice-shared (default) vs focused per-file search.**
   Default reuses PR-level `ReviewContext` sliced by path (cheap, no extra
   search calls). Focused per-file search (`TRUSTY_REVIEW_MAP_PER_FILE_SEARCH`)
   gives each file its own KG/APEX context but adds *N* search round-trips and
   latency. **Decision needed:** is sliced-shared context acceptable as the
   default, or do we want focused search on by default for review fidelity?

3. **Cross-file correctness depends on the synthesis pass.** The per-file map
   stage structurally **cannot** see cross-file compile-breaks (the #-rule at
   `prompt.rs:162`); only the reduce synthesis pass can, and it sees only symbol
   deltas, not full diffs. **Risk:** map-reduce could *regress* the compile-break
   detection that the unified pass gets "for free" when the whole diff fits.
   **Decision needed:** is the symbol-delta synthesis view sufficient, or must
   the reduce stage re-feed full diffs of files that delete/rename symbols
   (raising cost back toward unified)? This is the single biggest quality risk
   and should gate flipping the default in PR-6.
