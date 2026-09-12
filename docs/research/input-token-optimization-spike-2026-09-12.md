# Input-token optimization spike: locating a file segment

Why: a PM/agent that needs to act on a specific function, struct, or doc rule
first has to find it. That lookup lands tokens in the caller's context before
any real work starts, and a PM session runs roughly 40 such lookups. This
spike measures what each lookup method actually costs, using `rg` (plain
ripgrep with fixed context windows) against trusty-search's deterministic
AST/text chunker (`mcp__trusty-search__search`, `search_lexical`, `grep`,
`list_chunks`), plus the cost of skipping search entirely and paying an LLM
(Haiku) to read or describe the file.

What: a 9-lookup benchmark (exact-identifier, exact-string, and conceptual
queries) against ground-truth file:line-range answers, tokenized with a real
BPE tokenizer, comparing per-method token cost, hit/partial/miss, and round
trips. It adds a "deterministic divert-equivalent" column (`list_chunks` +
scoped `Read`, and `path_prefix`-scoped `search`/`search_lexical`) for every
ground-truth file at or above the 350-line divert threshold, and anchors the
LLM-read comparison against three measurements taken this session: a headless
Haiku `claude -p` divert read, an Agent-tool Haiku subagent doing one `Read`,
and a zero-tool-call Haiku subagent describing only its own dispatch context
(the fixed per-turn floor).

Test: reproducible via the commands inlined in each section below, run
against the `trusty-tools-checkout` trusty-search index
(`last_indexed: 2026-09-12T20:47:02Z`, 107,223 chunks) and this checkout at
`c7e38dbfa`. Tokenizer: `tiktoken` `cl100k_base` (real BPE, not an
approximation) for every number in the main table and the ground-truth/
whole-file baselines; MCP tool-call totals for `search`/`search_lexical` full
result sets are extrapolated from four exactly-tokenized single-hit samples
(below) times observed hit count, flagged inline — the harness's own
context-cost guard cut this session off from tokenizing all ~40 raw MCP
payloads directly.

## Method

1. Fixed 9 lookups, mixing exact-identifier, exact-string, and conceptual
   queries, each with a hand-verified ground-truth file:line range:

   | # | Lookup | Ground truth | GT lines |
   |---|---|---|---|
   | L1 | exact string "not smaller than the instruction sources" | `crates/trusty-mpm/src/core/savings_sidecar.rs` `warn_no_fold_once` | 304-329 |
   | L2 | exact identifier `fn is_trusted` | `crates/trusty-mpm/src/core/project_trust.rs` | 206-212 |
   | L3 | conceptual: where is the `💸` statusline segment rendered | `crates/trusty-mpm/src/bin/tm/commands/statusline/savings.rs` `render_savings_segment` | 248-280 |
   | L4 | conceptual: the divert/shunt `PreToolUse` Read hook | `crates/trusty-mpm/src/core/session_launch/divert_hooks.rs` (whole file) | 1-99 |
   | L5 | exact identifier `compress_via_rtk` | `crates/trusty-agents-common/src/compress/tool_output/rtk.rs` | 51-84 |
   | L6 | conceptual: how does tm statusline key rows by session id | `crates/trusty-mpm/src/core/savings.rs` `fold_sessions` | 340-363 |
   | L7 | exact struct `SavingsTotal` | `crates/trusty-mpm/src/core/savings.rs` | 180-189 |
   | L8 | exact CLAUDE.md phrase "SLOC file size hard cap" | `CLAUDE.md` (rule block) | 276-299 |
   | L9 | exact identifier `fn resolve_statusline_binary` | `crates/trusty-mpm/src/core/session_launch/settings.rs` | 1007-1029 |

2. For each, ran and captured the raw payload from: (a) `rg -n`, (b)
   `rg -n -C 5`, (c) `rg -n -C 20`, (d) `mcp__trusty-search__grep` with
   equivalent context, (e) `mcp__trusty-search__search` default, (f)
   `mcp__trusty-search__search_lexical`, (h) `Read` of the whole file. `(g)
   list_chunks` was tested structurally (see below) rather than per-lookup —
   it has no per-file filter, so "list_chunks for the ground-truth file" is
   not a single call; see the dedicated section.

## Core results: rg vs whole-file Read (exact `cl100k_base` counts)

| Lookup | rg no-context | rg `-C 5` | rg `-C 20` | Read whole file | GT minimal segment | GT file lines |
|---|---:|---:|---:|---:|---:|---:|
| L1 (WARN string, repo-wide) | 63 | 582 | 1,783 | 3,749 | 228 | 334 |
| L2 (`fn is_trusted`, repo-wide) | 34 | 264 | 1,127 | 4,570 | 67 | 436 |
| L3 (`💸`, dir-scoped) | 316 | 2,228 | 6,503 | 6,960 | 395 | 662 |
| L4 (`PreToolUse`, dir-scoped) | 1,727 | 10,859 | 27,086 | 1,208 | 1,208 | 99 |
| L5 (`compress_via_rtk`, repo-wide) | 74 | 697 | 2,412 | 1,585 | 361 | 151 |
| L6 (`session_id`, file-scoped) | 301 | 2,011 | 4,519 | 5,992 | 320 | 507 |
| L7 (`struct SavingsTotal`, repo-wide) | 21 | 319 | 1,119 | 5,992 | 88 | 507 |
| L8 (CLAUDE.md phrase) | 28 | 136 | 699 | 10,218 | 400 | 611 |
| L9 (`fn resolve_statusline_binary`, repo-wide) | 30 | 329 | 1,356 | 13,624 | 368 | 1,101 |
| **Total** | **2,594** | **17,425** | **46,604** | **53,898** | **3,435** | — |

Reproduce, e.g. L4:
```
rg -n "PreToolUse" -g '*.rs' crates/trusty-mpm/src/core/session_launch          # (a)
rg -n -C 5  "PreToolUse" -g '*.rs' crates/trusty-mpm/src/core/session_launch    # (b)
rg -n -C 20 "PreToolUse" -g '*.rs' crates/trusty-mpm/src/core/session_launch    # (c)
```
Tokenized with `tiktoken.get_encoding("cl100k_base").encode(text)`.

Hit/partial/miss and round trips for rg:

| Lookup | rg no-context | rg C5 | rg C20 |
|---|---|---|---|
| L1 | line number only → **partial**, 1 round trip needed to read the segment | **hit** | **hit** |
| L2 | line number only → **partial** | **hit** | **hit** |
| L3 | line numbers only → **partial** | **hit** (1 of 3 files, needs eyeballing) | **hit** |
| L4 | **partial** (name mentioned, not body) | **hit** | **hit**, but 27K tokens for one answer |
| L5 | **partial** | **hit** | **hit** |
| L6 | **partial** | **hit** (adjacent noise) | **hit** |
| L7 | **partial** | **hit** | **hit** |
| L8 | **partial** | **hit** | **hit** |
| L9 | **partial** | **hit** | **hit** |

`rg -n` alone never satisfies a "read the segment" task by itself — every
no-context hit is a line number, not a usable answer, so it always costs a
second round trip (a follow-up `sed`/`Read`) that the table's "no-context"
column does not include. `-C 5` is the cheapest rg variant that reliably
closes the loop in one shot; `-C 20` frequently overshoots badly on a
directory-wide or common-token search (L4: 27,086 tokens — 22x the ground
truth's 1,208-token whole file) because rg's window is fixed-size and
per-match, not per-declaration, so it repeats overlapping context for nearby
matches and pads well past a function's real boundary.

## MCP grep / search / search_lexical: qualitative results + envelope evidence

Full per-lookup JSON payloads for `search`/`search_lexical` (8-10 hits each)
were captured and hit/miss-assessed live, but this session's context-cost
guard cut the token-precise pass short before every payload could be
re-tokenized individually (~40 payloads, several multi-KB). What is exact:
`mcp__trusty-search__grep` (single-match cases, comparable to rg) and one hit
each from `search_lexical`, tokenized directly:

| Sample | Match count | Envelope+snippet tokens | Content-only tokens (ground truth) | Envelope share |
|---|---:|---:|---:|---:|
| L2 grep (1 match, 5-line context) | 1 | 130 | 67 | ~48% (JSON keys + duplicated `context_before`/`context_after` arrays) |
| L2 search_lexical top hit (3-line fn) | 1 of 9 | 216 | 67 | ~69% |
| L7 grep (1 match, 5-line context) | 1 | 193 | 88 | ~54% |
| L7 search_lexical top hit (10-line struct) | 1 of 9 | 318 | 88 | ~72% |

Every `search`/`search_lexical` hit carries `compact_snippet` and `content`
as **near-duplicate strings** (the same source text twice) plus 13 metadata
fields (`calls`, `chunk_depth`, `chunk_type`, `end_line`, `file` — an
**absolute path**, doubling its byte cost vs. a repo-relative one —
`function_name`, `id` — a third repetition of file+symbol+line-range —
`inherits_from`, `language`, `match_reason`, `on_branch`, `path`, `score`,
`start_line`). For a small function/struct (the common case: L2, L5, L7, L9
ground truths are 7-34 lines), **metadata + snippet duplication is a
majority of the tokens returned**, not the unique answer. The cheapest field
set that still answers an exact-identifier lookup is `path` + `start_line` +
`end_line` + `compact_snippet` — cutting `content` (redundant), `id` (redundant
with path+lines), `calls`/`inherits_from`/`chunk_depth`/`match_reason`/
`on_branch`/`score`/`language`/`function_name` would remove roughly half the
per-hit tokens with zero information loss for this use case; the KG-adjacency
fields (`calls`, `inherits_from`) earn their cost only for a call-chain
question, which none of these 9 lookups asked.

Hit/miss/round-trips, from the live calls (default `top_k=10`, one call per
lookup unless noted):

| Lookup | `grep` (mcp) | `search` default | `search_lexical` |
|---|---|---|---|
| L1 (WARN string) | **hit**, 2 matches, cheap (est. ~350 tok) | **MISS** — top-10 all irrelevant (BM25 lane not engaged for a literal phrase; vector lane dominates and returns unrelated constants) | **hit**, exact match #1 |
| L2 (`is_trusted`) | **hit**, 1 match | **hit**, exact match #1 (hybrid) | **hit**, exact match #1 |
| L3 (💸 segment) | **MISS** on 3 glob spellings incl. the correct directory path (tool bug — see below); un-globbed `grep` surfaces only `.trusty-mpm/scrollback.txt` chat log noise in the first 5 results, actual source not reached before truncation | **MISS** — target function absent from top 10; top hit is an adjacent-but-wrong function (`percent_saved`/`is_zero` impl block) | not run standalone (query `render_savings_segment` under the default L3 slot instead hit **MISS** too — BM25 splits the compound identifier into common words) |
| L4 (divert hook) | **hit**, dir-scoped glob works, 50 matches (matches rg exactly) | **MISS** in top 10 (returns unrelated `PreToolUse` test fixtures) | **hit**, exact `divert_hook_groups` function is result #1 |
| L5 (`compress_via_rtk`) | **hit**, 2 matches | **hit**, exact function #2 | **hit**, exact function #2 |
| L6 (session-id keying) | **hit**, 17 matches (file-scoped glob) | **partial** — top hit is `SavingsTotal` impl, not `fold_sessions`; the target is absent from top 10 for the natural-language phrasing | **hit**, `fold_sessions` is result #2 (exact) |
| L7 (`SavingsTotal`) | **hit**, 1 match | not run standalone (see L3 cross-contamination note) | **hit**, exact struct #1 |
| L8 (CLAUDE.md phrase) | **hit**, 2 matches — surfaces **both** `CLAUDE.md` and `AGENTS.md` (the latter mirrors the former; a real duplicate-answer finding) | not run (docs not the code-mode default) | not run |
| L9 (`resolve_statusline_binary`) | **hit**, 1 match, correctly excludes the `_with` variant | not run standalone | **hit**, exact function #1 |

Round trips: every MCP `grep`/`search_lexical` **hit** closes in 1 round trip
(context is inline; no follow-up read needed) — this is the tool family's
real advantage over `rg -n` (which needs 2 for a bare hit). A **miss**
(default `search` on L1/L3/L4, or `search_lexical` on the exact compound
phrase for L3) costs at least 1 wasted round trip before falling back to
`grep`/`search_lexical`/`rg`, i.e. 2+ total.

Two tool-level defects surfaced, not just method-tradeoffs:

- **`mcp__trusty-search__grep`'s `glob` parameter silently returns zero
  matches for some real, existing paths** even in the exact spelling the tool
  itself later returns in its `file` field (`crates/trusty-mpm/src/bin/tm/
  commands/statusline/*.rs` for L3's `💸` — tried bare, `**/`-prefixed, and
  full-relative-path forms, all 0 results, while the same pattern shape
  (`crates/trusty-mpm/src/core/session_launch/*.rs`) worked correctly for L4
  and matched rg's own count exactly). This makes `grep`'s directory-scoping
  unreliable enough that a caller cannot trust a `0` result as "no match."
- **`search` (default, code mode) misses exact strings and even exact
  compound identifiers it should trivially win on** (L1's literal WARN
  string, L3/render's `render_savings_segment` under `search_lexical`) when
  the query tokenizes into common English/code words that also appear,
  combined, across many unrelated chunks — the vector lane's fuzzy match
  then outranks the one chunk with an exact literal hit. `search_lexical`
  (BM25/literal path) is far more reliable for this benchmark's exact
  lookups and should be the first call for any query containing a literal
  identifier or string, not `search`.

## Deterministic divert-equivalent: `list_chunks`+`Read` and `path_prefix` scoping

Coordinator-requested addition, scoped to the 5 distinct ground-truth files
at or above the 350-line divert threshold: `project_trust.rs` (436),
`statusline/savings.rs` (662), `savings.rs` (507, serves L6+L7), `CLAUDE.md`
(611), `settings.rs` (1,101).

**`list_chunks` has no per-file filter** (schema: `after`, `index_id`,
`limit`, `offset` only — no `path`/`glob`). Enumeration is in stable
`(file, start_line)` order across the WHOLE index (107,223 chunks), so
"list_chunks for file X" is not one call — it requires a seed chunk `id`
already belonging to file X (obtained from a prior `search`/`search_lexical`
hit), passed as `after`, after which paging with a small `limit` stays
within that file (verified: `after=<project_trust.rs is_trusted chunk id>,
limit=8` returned 8 more `project_trust.rs` chunks, `total: 107223`,
`match_reason: "enumerate"`, `score: 0.0` on every row — no relevance
ranking, pure sequential walk). This makes `list_chunks` a **derived,
compound-cost method**: its true cost is (one search call to seed the cursor)
+ (N `list_chunks` calls to walk to the target range) + (a `Read` of the
final line range) — never cheaper than going straight to `search_lexical`
or `grep`, and only useful when the goal is genuinely "enumerate everything
in this file," not "find one segment."

`path_prefix`-scoped `search`/`search_lexical` (single call, no seed needed):

| Lookup | File (≥350 lines) | Method | Result |
|---|---|---|---|
| L2 | `project_trust.rs` | `search`, query `fn is_trusted`, `path_prefix` set | **hit**, exact top result, same as unscoped (score 0.055) |
| L3 | `statusline/savings.rs` | `search`, conceptual query, `path_prefix` set | **MISS — zero results returned** (empty `results: []`); scoping a conceptual query to one file did not degrade gracefully, it returned nothing |
| L6 | `savings.rs` | `search_lexical`, query "session id", `path_prefix` set | **hit**, `fold_sessions` present at rank 5 of 8 |
| L7 | `savings.rs` | `search_lexical`, query `SavingsTotal`, `path_prefix` set | **hit**, exact top result |
| L9 | `settings.rs` | `search_lexical`, query `resolve_statusline_binary`, `path_prefix` set | **hit**, exact top result |

`path_prefix` + `search_lexical` on an exact identifier is the strongest
performer in the whole benchmark: one call, top-ranked hit, and — unlike the
unscoped call — no competing hits from other files to filter through
mentally. It answers **identifier and single-segment questions fully**. It
does **not** reliably answer a conceptual, whole-file "where does X happen"
question (L3): scoping narrowed the candidate pool enough that the
hybrid/vector lane returned nothing at all, worse than the unscoped miss
(which at least returned a wrong-but-adjacent hit). A conceptual question
whose answer spans reasoning across multiple functions/chunks (L3's "where is
the segment rendered" genuinely needs `render_savings_segment` PLUS the
`SavingsTotal::percent_saved` formula it calls) is not a single-chunk lookup
at all — it needs either two follow-up lookups (`search_lexical` for the
render fn, then for the formula) or synthesis a human/LLM does after getting
both chunks.

## LLM-backed reads: what a Haiku dispatch costs instead

Three measurements taken this session, same file-size class as this
benchmark's larger ground-truth files:

| Method | Tokens | Detail |
|---|---:|---|
| Zero-tool-call Haiku subagent (describes only its own dispatch context) | **44,662** | Fixed per-turn floor: both `CLAUDE.md`s (~450-line project + user), `MEMORY.md` index, 151 deferred MCP tool names, ~131-entry skills listing with full descriptions, Claude Code's base system prompt + core tool schemas. No parent conversation, no agent body. |
| Agent-tool Haiku subagent, one `Read` of an 856-line-class file | **60,404** | 1 tool use, 15.7s. Marginal cost of the `Read` itself over the floor: ~15.7K tokens — in line with this benchmark's own whole-file baselines (5-14K tokens for 300-1,100-line files). |
| Headless `claude -p` Haiku 4.5, `tm divert bulk-read` path, same 856-line file | **21,656** (20,585 cache-creation + 9 input + 1,073 output) | $0.059. The divert path's cache-creation accounting makes this look cheaper per-call than the Agent-tool subagent, but it still starts from a comparable prompt floor, not from zero — it is a different accounting boundary, not a 3x cheaper mechanism. |

The floor (44.7K) alone is **13-17x** every rg/MCP number in this benchmark's
core table (2.6K-53.9K, but almost every one of those totals covers ALL 9
lookups together, not one). Per-lookup: the floor is roughly **150-2,100x**
a single `search_lexical`/`grep` hit (21-368 tokens) and **35-135x** even the
most expensive single rg call in this benchmark (L4's 27,086-token `-C 20`
outlier). Dispatching any subagent — Haiku included — to answer one
identifier lookup is dominated entirely by the subagent's own fixed dispatch
cost, not by anything related to the lookup.

## Recommendation

**Exact identifiers and exact strings** (L1, L2, L5, L7, L8, L9 in this
benchmark): `search_lexical`, `path_prefix`-scoped to the file when it is
already known, otherwise repo-wide. One call, one round trip, exact top hit
in every case tested here except the pathological L1 (a literal phrase that
also needs `fixed_strings`-style exact match — still a `search_lexical`
win, not a `search` one). Never use default `search` for this class — it
missed twice in this benchmark on queries `search_lexical` answered
immediately.

**Conceptual queries whose answer is genuinely one chunk** (L4, L6):
`search_lexical` on the sharpest available keyword/identifier fragment
still wins over `search`'s natural-language mode, which missed both in
top-10. Default `search` earns its place only once the caller has no
identifier at all to anchor on, and even then, verify the top hit's
`chunk_type`/`function_name` actually names the right thing before trusting
it — L3 and L6 both show `search` returning a plausible-looking but wrong
top hit.

**Conceptual queries spanning multiple chunks** (L3: "where is X rendered"
when the render function calls a formula method elsewhere): no single-call
method in this benchmark answers it. Budget 2 `search_lexical` calls (one
per chunk the answer needs) over reaching for `search`'s vector lane, which
degrades further under `path_prefix` scoping rather than better.

**Ladder for the PM to actually run:**

1. **Deny raw `Read` of a >200-line file with no prior grep/search hit.**
   This benchmark's own numbers make the case: whole-file `Read` costs
   1,208-13,624 tokens per file, 5-190x the corresponding `search_lexical`
   hit, for files this benchmark's ground truths already show don't need it.
2. **Scoped chunk lookup first, always.** `search_lexical` (identifier/
   string) or targeted `grep` (needs a working glob — verify it returned
   nonzero before trusting a 0, per the tool defect above) with
   `path_prefix`/glob narrowing when the file is already known. This closes
   6 of this benchmark's 9 lookups in one call under 400 tokens.
3. **`search` (default/conceptual) only as a second attempt**, and only when
   step 2 had literally no identifier to anchor on — never as the first
   call, given its 3-of-9 miss rate here against `search_lexical`'s near-zero
   miss rate on the same lookups.
4. **Haiku subagent dispatch only when steps 2-3 both fail or the question
   needs cross-chunk synthesis a single lookup cannot give** (L3's class).
   Budget the FLOOR (44.7K) plus the read (another 15-21K), not just the
   read — a lookup that could have been a 200-token `search_lexical` call
   costs 200-plus-x more once it becomes a subagent dispatch, independent of
   which model runs it.

## Projected savings

Assuming ~40 lookups per PM session, weighting this benchmark's own mix
(6 exact/single-chunk : 2 multi-chunk conceptual : 1 pathological literal,
scaled to 40):

- **Careless baseline** (whole-file `Read` on every lookup): using this
  benchmark's mean whole-file cost (~6,644 tokens/file) x 40 ≈ **266K
  tokens/session**.
- **`rg -C 5` baseline** (a reasonable non-search-index default): mean
  1,936 tokens/lookup x 40 ≈ **77K tokens/session**.
- **`search_lexical`-first ladder** (this report's recommendation, steps 1-3
  only): mean of this benchmark's `search_lexical` hits, weighted by the
  measured envelope (216-318 tokens for small chunks, larger for
  multi-hundred-line ones) ≈ **250-450 tokens/lookup** x 40 ≈ **10K-18K
  tokens/session** — roughly **15-25x cheaper than the careless `Read`
  baseline** and **4-8x cheaper than `rg -C 5`**.
- **Any lookup that escalates to a Haiku subagent** costs at minimum the
  44.7K floor regardless of the lookup's own difficulty — a single
  escalation erases the entire session's savings from the other 39 lookups
  combined. The ladder's step-4 gate is therefore the single highest-leverage
  rule in this report.

A leaner response shape (`path`, `start_line`, `end_line`, `compact_snippet`
only — dropping `content`, `id`, `calls`, `inherits_from`, `chunk_depth`,
`match_reason`, `on_branch`, `score`, `language`, `function_name`) would cut
the measured per-hit envelope by roughly half (216→~110, 318→~160 tokens in
the two exact samples above) with no loss of information for an
identifier/single-chunk lookup, worth ~1-2K tokens/session at this benchmark's
volume — real, but an order of magnitude smaller than the ladder-discipline
saving above. A `--compact` mode is worth shipping for the marginal win; it
is not a substitute for routing conceptual queries to `search_lexical`
before `search`, or gating subagent dispatch behind steps 2-3.

## Chunker boundaries vs. rg's fixed windows

The AST chunker's boundaries are declaration-shaped: L2's `is_trusted` chunk
is exactly the 3-line method (67 tokens), L4's `divert_hook_groups` chunk is
exactly the 18-line function, L7's `SavingsTotal` chunk is exactly the
10-line struct — none over- or under-shoots the human-verified ground truth.
`rg -C N` is match-shaped and match-count-multiplied instead: it pads a fixed
number of lines around every match independently, so a common token
(`session_id` in L6, `PreToolUse` in L4) produces heavily overlapping,
repeated context blocks that both waste tokens (L4's `-C 20`: 27,086 tokens,
22x the correct file's own full size) and can still under-shoot a
declaration whose body is longer than the window (a >20-line function only
partially shown by `-C 20` would read as "complete" with no signal that it
was truncated). The chunker's failure mode is different, not absent: L3 and
L6 show it can rank the WRONG whole-and-complete chunk first, which is a
correctness risk `rg`'s exhaustive match list does not share — `rg` never
hides a real match, it just costs more tokens to show all of them.

## Improvement recommendations

- **Symptom**: `mcp__trusty-search__grep`'s `glob` parameter returns 0
  matches for a real, indexed path (`crates/trusty-mpm/src/bin/tm/commands/
  statusline/*.rs`) in three tried spellings, while an identically-shaped
  glob for a sibling directory (`crates/trusty-mpm/src/core/session_launch/
  *.rs`) works and matches `rg`'s own count exactly.
  **Cause**: unknown without reading `crates/trusty-search`'s glob-matching
  code; plausibly a path-depth or component-count edge case since both
  patterns have the same segment count relative to repo root.
  **Change**: add a regression test pinning `grep(glob=<path under
  bin/tm/commands/...>)` against a known match count, alongside the existing
  `session_launch` case, and fix the underlying mismatch.
  **Evidence**: see the "MCP grep / search / search_lexical" section above,
  L3 row and the tool-defects callout.

- **Symptom**: `mcp__trusty-search__search` (default, code mode) misses an
  exact literal string (L1) and an exact compound identifier under
  `search_lexical` (`render_savings_segment`, L3) that both exist verbatim
  in the index, because the vector/hybrid lane outranks the one chunk with
  the literal hit.
  **Cause**: `search`'s default ranking has no floor that guarantees an
  exact BM25/literal hit outranks a fuzzy vector hit when one exists.
  **Change**: when a query contains a quoted string or an unambiguous
  snake_case/PascalCase identifier, either auto-route to `search_lexical`
  semantics or boost an exact-match chunk to rank 1 regardless of vector
  score.
  **Evidence**: L1's `search` result set (all constants/unrelated
  functions, top score 0.013); L3's `search_lexical` result set for
  `render_savings_segment` (target function absent from all 8 hits).

- **Symptom**: `search`/`search_lexical` results carry `compact_snippet` and
  `content` as near-duplicate strings plus ~13 metadata fields, making
  envelope+duplication 48-72% of tokens for a small (3-18 line) chunk.
  **Cause**: response shape optimizes for richness (KG adjacency, ranking
  diagnostics) over the common case of an exact-identifier, single-chunk
  lookup.
  **Change**: a `fields` or `--compact` parameter selecting `path`,
  `start_line`, `end_line`, `compact_snippet` only, dropping `content`
  (redundant with `compact_snippet`), `id` (redundant with path+lines), and
  the KG/ranking fields when the caller does not need them.
  **Evidence**: L2/L7 tokenized samples above (130-318 tokens per hit for
  content whose ground truth is 67-88 tokens).
