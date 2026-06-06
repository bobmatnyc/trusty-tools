# Token Compression Techniques and Caveman NLP for LLM Harnesses

**Date**: 2026-05-01
**Context**: open-mpm Rust agent harness — survey of token compression techniques with emphasis on
Rust-applicable, rule-based approaches. Includes analysis of rtk-ai/rtk and codejunkie99/ztk repos.

---

## 1. Repository Analysis

### rtk-ai/rtk — "Rust Token Killer"

**Status**: Exists. Active project. 39,315 stars. MIT license. Single Rust binary, zero dependencies.

**What it does**: CLI proxy that intercepts shell command output before it reaches an LLM context
window. It does NOT compress prompts or chat history — it compresses the *output of shell commands*
(git diff, cargo test, ls, grep, etc.) before the LLM sees the tool result.

**Claimed compression**: 60–90% on typical dev session command outputs. Their README table for a
30-minute Claude Code session projects ~118K raw tokens → ~24K tokens (-80%) overall.

**Techniques** (from source):

1. **Domain-specific structured filtering**: Each supported command gets a hand-written filter
   module (`src/cmds/git/`, `src/cmds/files/`, `src/cmds/tests/`, etc.). Not generic NLP — each
   filter knows the grammar of its command output.

2. **Noise removal**: For `git status`, strips file permission lines, index metadata, merge-base
   headers. For `cargo test`, strips passing test lines — only failures and the summary survive.

3. **Diff condensation** (`src/cmds/git/diff_cmd.rs`): Parses unified diff format. Replaces context
   lines with a summary header `+N added, -N removed, ~N modified`, keeps only changed lines.
   No context lines unless explicitly requested.

4. **Smart file reading** (`rtk read --level aggressive`): Signature extraction — strips function
   bodies, keeps declarations, type signatures, doc comments. This is language-aware AST-lite
   parsing, not generic NLP.

5. **Deduplication across session** (`rtk git status` repeated): Uses a TTL cache (30s for
   fast-changing commands, 2min for test runners, 5min for git log). Repeated identical output
   replaced with "[unchanged since Ns ago]".

6. **Grouping**: File listings grouped by directory. Grep results grouped by file. Log errors
   grouped by type rather than listed individually.

**Applicability to open-mpm**: High for tool-result compression (when sub-agents run shell
commands). Low for prompt/history compression — rtk does not touch LLM prompts. The source is pure
Rust and the techniques are extractable as a pattern library.

**Install**: `brew install rtk` or `cargo install --git https://github.com/rtk-ai/rtk`

---

### codejunkie99/ztk — "Zig Token Killer"

**Status**: Exists. 115 stars. Single Zig binary, 260KB. Clearly inspired by rtk.

**What it does**: Same positioning as rtk — CLI proxy compressing shell command output. Zig
implementation. Six-stage pipeline. Also adds a session memory cache with per-command TTLs.

**Claimed compression**: 90.6% overall across a real 256-command session (5.8M tokens saved).
Specific: `git diff HEAD~5` 92K → 18K tokens; `ls -la src/` 2K → 53 tokens; all-passing
`cargo test` 397 → 21 tokens.

**Techniques** (from source):

1. **Comptime filter dispatch** (`src/filters/comptime.zig`): All filter specs baked at compile
   time with XxHash64 hash dispatch. Collision-checked at compile time. Same pattern as rtk —
   per-command filter functions.

2. **Data format preservation**: `filterCat` checks if input is JSON/YAML/TOML before applying
   any filter. If detected, passes through unchanged. Smart avoidance of compressing data.

3. **Comment stripping**: `isCommentOnly` removes `//`, `#`, `--`, `/*` lines from code files.

4. **Blank line compression**: Multiple consecutive blank lines collapsed to one.

5. **Signature extraction** (`files_cat_aggressive.zig`): For code files >500 bytes, attempts
   function signature extraction before falling back to comment-strip-only.

6. **Session memory** (`mmap'd cache`): Per-command TTL cache. Repeated unchanged command output
   returned as a single-line "[unchanged]" token. Mutation commands (`git add`, `git commit`)
   invalidate related cache entries.

7. **Stderr policy routing** (`executor.zig`): Four modes — filter stdout only, filter stderr
   only, filter both independently, or merge-then-filter. Allows test runners (which write to
   stderr) to be compressed correctly.

8. **16MB cap with graceful fallback**: Outputs exceeding 16MB get a sentinel message rather than
   blowing up the pipeline.

**Differences from rtk**: Zig vs Rust. ztk is smaller (260KB vs rtk's binary). ztk has the
TTL-based session memory built in. rtk has more coverage (100+ commands vs ztk's ~45 explicit
filters + 25 regex-based). rtk is more mature (39K vs 115 stars).

**Applicability to open-mpm**: Same as rtk — excellent pattern reference for tool-result
compression. Not applicable to prompt or history compression directly.

---

## 2. Top Token Compression Techniques

### Technique 1: Tool-Output Domain Filtering (rtk/ztk pattern)

**What it is**: Intercept tool call results before inserting into LLM context. Apply command-aware
filters that strip noise and summarize signal.

**Compression ratio**: 70–90% on structured command outputs (test runners, git commands, directory
listings). Lower (30–50%) on arbitrary text output.

**Tradeoff**: Requires per-command filter logic. High implementation effort for full coverage but
individual filters are simple and additive. Errors in filters can drop signal — failures must
always pass through.

**Rust implementation sketch**:
```rust
trait ToolOutputFilter {
    fn filter(&self, raw: &str) -> String;
    fn matches(&self, tool_name: &str, args: &[&str]) -> bool;
}

struct TestRunnerFilter;
impl ToolOutputFilter for TestRunnerFilter {
    fn filter(&self, raw: &str) -> String {
        // Keep only FAILED lines + summary line, strip passing tests
        let failures: Vec<&str> = raw.lines()
            .filter(|l| l.contains("FAILED") || l.contains("error") || l.starts_with("test result"))
            .collect();
        if failures.is_empty() {
            format!("[all tests passed, {} lines suppressed]", raw.lines().count())
        } else {
            failures.join("\n")
        }
    }
    fn matches(&self, tool_name: &str, _: &[&str]) -> bool {
        matches!(tool_name, "bash" | "shell") // applied at result-processing time
    }
}
```

---

### Technique 2: Deduplication / Cross-Turn Redundancy Elimination

**What it is**: Hash sentences or paragraphs across the assembled prompt. On collision, replace
duplicate with a back-reference or drop entirely. Most impactful for agent harnesses that
re-inject skill files and system prompt fragments on every turn.

**Compression ratio**: 20–40% on long agent sessions. Near-zero overhead.

**Tradeoff**: Very low semantic risk. Must protect first occurrence.

**Rust**: `std::collections::HashSet<u64>` with xxhash or FNV hash of each sentence. Pure stdlib,
no crates needed.

---

### Technique 3: Sliding Window + Pinned Turn 0 (Session History)

**What it is**: Discard middle conversation turns. Always keep: (a) turn 0 (original user request)
and (b) the last N turns. This is the most impactful technique for multi-turn agent sessions.

**Compression ratio**: 40–70% on sessions >10 turns. Exact reduction depends on session length.

**Tradeoff**: Loses intermediate reasoning context. Mitigated by pinning turn 0. Not suitable if
the agent needs to reference a specific earlier turn by index.

**Rust**:
```rust
struct TokenBudget {
    max_context_tokens: usize,
    system_reserved: usize,
    pinned: Vec<ChatMessage>,     // [turn_0, ...always-keep...]
    window: VecDeque<ChatMessage>, // last N turns, pop front when over budget
}
```

---

### Technique 4: TF-IDF Sentence Filtering (Extractive Summarization)

**What it is**: Score each sentence in a prompt section by TF-IDF weight relative to the current
query. Discard sentences below a threshold. Retains high-information content, drops filler.

**Compression ratio**: 30–50% on verbose prose context. Degrades on dense technical content where
most sentences are high-TF-IDF.

**Tradeoff**: Medium semantic risk. Must exempt code blocks, JSON, tool definitions.

**Rust crates**: `rust-tfidf 1.1.1` (27K downloads) + `tfidf-text-summarizer 0.0.3` (4.3K
downloads, pure Rust).

---

### Technique 5: Stop Word / Function Word Removal ("Caveman NLP")

**What it is**: Remove articles (a, an, the), prepositions (of, in, at, by, for, from, with),
filler conjunctions (however, furthermore, therefore). The output reads like compressed cable news
or a Hemingway telegram.

Example: "The Python engineer should write a script that reads from the input file and outputs to
the console" → "Python engineer write script reads input file outputs console"

**Compression ratio**: 15–25% on natural-language prompts. ~5% on code-heavy prompts.

**Tradeoff**: Low–medium semantic risk. High risk if applied to instructions containing negation
("do NOT", "must not"). These must be protected explicitly. JSON, code blocks, and tool definitions
must be exempted.

**Rust crates**: `stop-words 0.10.0` (1.5M downloads, 40+ languages, pure Rust).

**Negation protection** (critical):
```rust
fn protect_negations(text: &str) -> String {
    // Collapse "do not" → "NOT", "must not" → "MUST_NOT" before stop-word removal,
    // then restore after. Never drop "not", "never", "no" from instructions.
    text.replace("do not", "NEGATION_DO_NOT")
        .replace("must not", "NEGATION_MUST_NOT")
        // ... apply stop word removal ...
        // ... restore NEGATION_ prefixes ...
}
```

---

## 3. Caveman NLP — Function Word Dropping

**Mechanism**: Drop all tokens in a predefined set: articles (the/a/an), most prepositions
(of/in/at/by/from/with), weak conjunctions (and/or at sentence boundaries), discourse markers
("As I mentioned", "It is worth noting that", "To summarize,").

**Inspiration**: Telegram language, military brevity codes, early SMS compression. Academic framing
comes from "selective context" approaches and LLMLingua's coarse-filter stage.

**Practical compression**: 15–25% raw. Stacks additively with deduplication and sentence filtering
for a combined 40–55% pipeline.

**Quality tradeoff**:
- Degradation on ambiguous pronouns: "give it to them" → "give them" (meaning shifts slightly)
- Safe on structured agent prompts where sentences are imperative and subject is explicit
- Unsafe on legal/contractual language where prepositions change meaning ("liable for" vs "liable
  to" vs "liable with")

**For open-mpm specifically**: Agent system prompts and skill files are imperative technical prose.
Function-word dropping is relatively safe here. Example:

Before: "The python-engineer agent should be given the task of writing a script that reads the
input data from the specified file path."

After: "python-engineer agent: write script reads input data from specified file path."

Token reduction: ~35% on this example.

---

## 4. Rust-Feasible Techniques (No GPU Required)

| Technique | Rust Crates | Effort | Reduction |
|---|---|---|---|
| Tool output filtering (rtk pattern) | stdlib only | High | 70–90% on tool output |
| Deduplication | stdlib HashSet | Low | 20–40% |
| Sliding window + pinned turn 0 | stdlib VecDeque | Low | 40–70% sessions |
| Stop word removal | `stop-words 0.10` | Low | 15–25% |
| Discourse marker removal | `regex 1.x` | Low | 3–8% |
| TF-IDF sentence filtering | `rust-tfidf 1.1` | Medium | 30–50% prose |
| Comment stripping (code files) | stdlib | Low | 10–30% code context |
| Blank line compression | stdlib | Trivial | 2–5% |

**Not feasible without GPU**: LLMLingua Stage 2 (perplexity-based token scoring), RECOMP,
AutoCompressor, any attention-based pruning. These require a small LM (LLaMA-7B class).

**LLMLingua Stage 1** (deterministic preprocessing) IS feasible in Rust: token budget allocation
+ stop word removal + coarse sentence scoring. Stage 1 alone achieves ~30–40% of LLMLingua's
reported compression.

---

## 5. Existing Rust Crates

| Crate | Version | Downloads | Purpose |
|---|---|---|---|
| `stop-words` | 0.10.0 | 1.5M | Stop word lists, 40+ languages |
| `rust-tfidf` | 1.1.1 | 27.6K | TF-IDF scoring |
| `tfidf-text-summarizer` | 0.0.3 | 4.3K | Extractive summarization via TF-IDF |
| `tiktoken-rs` | latest | 290K | Accurate BPE token counting (cl100k_base) |
| `regex` | 1.x | 250M+ | Pattern-based filler removal |
| `copyforward` | 0.2.1 | 356 | Cross-message substring dedup (early-stage) |

No Rust crate covers the full rtk/ztk command-output filtering approach — those are hand-rolled
per-command filters. The crates above cover the generic NLP compression layer.

---

## 6. Recommended Implementation for open-mpm

**Priority 1 — Tool result compression** (highest ROI, most unique value): When sub-agents return
tool call results (bash output, file reads), run them through a filter before inserting into the
next LLM turn. Start with: test-runner filter (suppress passing tests), diff filter (drop context
lines, keep summary header + changed lines), file-read filter (strip comment-only lines, collapse
blanks).

**Priority 2 — Session history sliding window**: `TokenBudget` struct with pinned turn 0 + last 8
turns. Configurable via `[llm] context_window = 8` in agent TOML.

**Priority 3 — System prompt / skill file dedup**: Before assembling the final prompt, hash all
injected sections. Drop exact duplicates. This is O(n) with a HashSet and zero semantic risk.

**Priority 4 — Stop word removal on prose context** (optional): Apply to the "context" sections of
prompts only, never to instructions, tool definitions, or code blocks.

**Combined expected reduction**: 50–65% on a typical agent session mixing tool calls with prose
context. Tool-result filtering alone can hit 70–80% on code-heavy workflows (the dominant case for
open-mpm).

---

## 7. Prior Art Reference

- **LLMLingua** (Microsoft, 2023): github.com/microsoft/LLMLingua — Stage 1 (deterministic) +
  Stage 2 (neural perplexity scoring). Stage 1 alone: ~30–40% reduction.
- **Selective Context** (Guo et al., 2023): TF-IDF + stop word removal pipeline. 50% reduction
  reported on chat contexts with <5% QA performance degradation.
- **rtk-ai/rtk**: Production Rust binary, 39K stars, best reference for command-output compression.
- **codejunkie99/ztk**: Zig binary, smaller but same pattern, good for TTL-cache session memory
  design.

---

## Summary

rtk and ztk both exist and are production-quality tools, but they solve a different problem than
prompt compression: they compress *tool call outputs* (shell command results), not the LLM prompt
or chat history itself. For open-mpm, adapting the rtk/ztk filtering pattern for sub-agent tool
results is the single highest-ROI compression opportunity — 70–90% reduction on test and build
output.

For prompt/history compression without a GPU: combine deduplication (20–40%) + sliding window
(40–70% on sessions) + stop word removal (15–25%) for a combined 45–60% reduction. All
implementable with pure Rust crates and stdlib. Avoid neural approaches unless you can afford a
small model call as an optional compression pass.
