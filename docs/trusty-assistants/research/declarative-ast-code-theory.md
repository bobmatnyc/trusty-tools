# Declarative AST↔Code: Theory and Implications for open-mpm

**Date:** 2026-05-07
**Status:** Research
**Scope:** Theoretical foundations and architectural implications for treating
the symbol registry / AST as the source of truth and source files as a derived,
goal-conditioned projection.
**Code reviewed:** `crates/symgraph/src/{emitter.rs,registry.rs,graph.rs}` at HEAD.

---

## Executive Summary

- **Hypothesis 1 holds in the limit, but not in practice.** For an idealised
  language with a fixed grammar and a canonicalising formatter, `parse ∘ fmt`
  is a well-behaved *quotient lens* (Foster, Pilkiewicz, Pierce 2008): two
  sources differing only in whitespace, comment positions, or sibling order
  collapse to the same AST. Real languages (Rust `syn`/`quote`, C preprocessor,
  Python `ast.unparse`, Go `go/printer`) leak non-AST information — comments,
  doc strings, attribute order, derive-macro expansion, conditional compilation —
  that breaks pure bijection. The right framing is therefore: **code is a
  serialisation of an AST *plus* a small, well-typed bag of "ignorable"
  metadata**, exactly the situation quotient lenses model.
- **Hypothesis 2 is plausible and under-explored.** No paper found combines
  goal-conditioned RL, AST representation learning, and emit (as opposed to
  synthesis from a spec). The closest prior art — Neo (PLDI 2018), code2vec /
  code2seq, NeurIPS 2024 goal-conditioned reward models, Sketch/Rosette — each
  cover one axis but never the triple. The free variable that becomes trainable
  if `emit` is decoupled from "match the original bytes" is the *layout policy*:
  file partitioning, ordering within file, naming, comment regeneration.
- **open-mpm's emitter is already half-way there.** `emitter::emit()` is a pure
  function `(SymbolRegistry, LayoutRules) → HashMap<PathBuf, String>`. The
  *strategy* is hard-coded (lexicographic file assignment, topo sort with
  ID-based tie breaking, sorted imports). Lifting this to a trait
  `EmitStrategy` with three method groups — partition, order, render — gives a
  clean substitution surface for either hand-written alternative strategies or
  a learned model, with no caller-side change.
- **Theoretical bound on "optimal emit" is set by the AST's information
  content, not by emit-time cleverness.** Comments, naming intent, blank-line
  cadence, the *order in which a human discovered the design*, and the test
  case that motivated each branch — these are not in the registry today. Any
  goal-conditioned emitter is bounded above by what the registry preserves;
  this argues for *enriching the registry with provenance* before training a
  smarter emitter.
- **Recommended next experiments:** (a) ship `EmitStrategy` trait now (cheap,
  unblocks experimentation); (b) add a `comments: Vec<Trivia>` field to
  `SymbolEntry` to break the bijection-breaking floor; (c) define three
  hand-written strategies (current/canonical, performance-grouped, LLM-
  readable) before any training to characterise the design space; (d) only
  then explore learned strategies, conditioned on test pass rate as the
  reward signal.

---

## Part 1: Code → AST Declarativity

### 1.1 PL Theory Perspective

**Denotational semantics** is unambiguous on this point: the *denotation* of a
program is its meaning function `⟦·⟧ : Program → Domain`, not its source text
nor its AST. Two ASTs with different shapes can have the same denotation
(e.g. `x + 0` and `x`); two source strings with different bytes can produce
the same AST (different whitespace, comment placement). So neither "code" nor
"AST" is automatically the *semantic* source of truth — they are increasingly
abstract syntactic representations of an underlying mathematical object.

For the open-mpm question, the relevant reduction is the *syntactic* one:

```
        parse                fmt
source ────────► AST ◄──────── source'
```

With `fmt` standing for a canonicalising formatter (rustfmt, gofmt, black),
the question "is `parse(fmt(s))` pure and total?" is equivalent to asking
whether `fmt` is a *retraction* onto a normal-form sublanguage `S_norm ⊂ S`
that the parser still accepts. In well-designed languages this is true:
gofmt, rustfmt, and black are explicitly designed so that
`fmt(fmt(s)) == fmt(s)` (idempotence) and `parse(fmt(s)) == parse(s)`
(semantics-preserving).

**Attribute grammars** (Knuth 1968) extend context-free grammars with
synthesised and inherited attributes computed from parse trees. They are the
classical framework for arguing that an AST plus its decorating attributes is
a *complete* representation of a program for a given semantic concern (type
checking, code generation). They do *not* claim AST is complete for the
*social* concerns of code (style, comments, learnability) — those live in a
different attribute space.

**Normalisation by evaluation (NbE)** offers a tighter analogy. NbE recovers
a normal-form syntactic object by evaluating into a semantic domain and then
*reifying* back to syntax. The "AST + canonical formatter" pipeline is
essentially a syntactic NbE: parse evaluates into the AST domain, fmt reifies
back to a normal source form. The connection is informal (no actual semantic
domain in the loop) but the *shape* of the construction is the same: pick a
domain in which equivalent terms have a unique representative, project into
it, project back.

### 1.2 Bidirectional Transformations and Lenses

The lens framework (Foster, Greenwald, Moore, Pierce, Schmitt 2007 — TOPLAS
"Combinators for bidirectional tree transformations") is the single most
relevant body of theory.

A lens `l : C ⇌ A` between concrete and abstract states is a triple
`(get, put, create)` satisfying:

| Law | Statement | Meaning for parse/emit |
|-----|-----------|------------------------|
| GetPut | `put(c, get(c)) = c` | If you parse a file then re-emit with no changes, you must get the original bytes back. |
| PutGet | `get(put(c, a)) = a` | If you emit an AST then re-parse, you must recover the same AST. |
| PutPut | `put(put(c, a), a') = put(c, a')` | A second edit overrides the first; no hysteresis. |

For a bare `(parse, emit)` pair on real-world Rust source, **PutGet is the
only law that holds unconditionally** — emit then parse should produce the
same AST. **GetPut fails** because comments, whitespace, and blank-line
cadence are discarded by parse and cannot be reconstructed by emit. **PutPut
holds** trivially when emit is a pure function of the AST.

This is exactly the situation **quotient lenses** (Foster, Pilkiewicz, Pierce
2008 — "Quotient Lenses", ICFP) were invented for. A quotient lens replaces
strict equality in GetPut with equality *modulo a chosen equivalence relation
on C* — typically "ignore whitespace" or "ignore comment position". Under
this relaxation, `parse ∘ emit` becomes a well-behaved bijection on the
quotient `C / ~`. The research question "is code → AST 100% declarative?"
becomes the engineering question "**how rich is the equivalence relation we
declare on source files?**" — and that is a knob, not a fact.

**Boomerang** (Bohannon, Foster, Pierce, Pilkiewicz, Schmitt 2008 — POPL
"Boomerang: Resourceful Lenses for String Data") operationalises this for
text data with a combinator language. Each combinator simultaneously
specifies a parser and a printer; well-typed compositions are guaranteed to
satisfy the lens laws. Boomerang is the most direct prior art for what an
"emit-aware AST" would look like: every node carries enough context for its
own re-printing.

**Symmetric lenses** (Hofmann, Pierce, Wagner 2011) and **edit lenses**
(Hofmann, Pierce, Wagner 2012) generalise to the case where both sides hold
information the other lacks — the natural model when comments/formatting
live alongside the AST as a parallel channel.

### 1.3 Where Real Languages Break the Bijection

| Language feature | What's lost on `parse` | Recoverable? |
|------------------|------------------------|--------------|
| Comments (line/block) | Position, attached node ambiguous | Only if AST stores trivia (rust-analyzer's `SyntaxNode`, Roslyn red-green trees do; `syn` and Python `ast` do not by default). |
| Doc strings | Same as comments unless typed | Partially — `///` in Rust is captured as `#[doc = ...]`. |
| Whitespace, blank lines | All gone | No. Formatters synthesise a *policy*, not the original. |
| Macro expansion (`macro_rules!`, `#[derive]`, C `#define`) | Pre-expansion source not in expanded AST | No without storing the macro invocation as the AST node. |
| Conditional compilation (`#[cfg]`, `#ifdef`) | Inactive arms typically dropped | Only if parser preserves both. |
| Attribute order (`#[a] #[b]` vs `#[b] #[a]`) | Often canonicalised | Lossy by design. |
| Identifier renaming hints (`x_temp` vs `tmp`) | Captured but rationale lost | The string survives; the *intent* doesn't. |
| Operator parenthesisation (`a + (b * c)` vs `a + b * c`) | Identical AST after precedence | No, and arguably correct to lose. |
| Trailing commas, optional semicolons | AST identical | No. |
| Sibling order in unordered constructs (use list, struct fields when not positional) | AST may preserve, formatter may sort | Depends on lens declaration. |

Concrete tooling status:

- **Rust `syn`** discards trivia; `quote!` cannot reconstruct comments.
  rust-analyzer uses Roslyn-style red-green trees (`rowan` crate) which *do*
  preserve trivia and are the right substrate for a lens-respecting emitter.
- **Python `ast.unparse`** (PEP 594, stdlib since 3.9) is explicitly
  documented as not round-trip-stable for whitespace; `libcst` is the
  community answer for concrete-syntax-tree round-tripping.
- **Go `go/printer` + `go/ast`** is the cleanest of the mainstream cases:
  comments are first-class `*ast.CommentGroup` attached to the file, and
  `gofmt`'s output is a fixed point.
- **C preprocessor** is the worst case; the only correct response is to
  treat preprocessed and unpreprocessed source as separate concrete sides of
  two stacked lenses.

### 1.4 Verdict: How Declarative Is It?

The honest framing:

> `(parse, emit)` is a **quotient lens on a chosen equivalence**, not a pure
> bijection on bytes. The size of the equivalence class — what you choose to
> call "the same code" — is a design choice. open-mpm has implicitly chosen
> *very large* classes (the registry stores no comments, no whitespace
> intent, no doc-comment provenance), which is fine for a synthesis-focused
> tool but means the current setup cannot losslessly absorb hand-edited
> source.

**Practical conclusion for open-mpm:** treat the registry as the abstract
side, source files as the concrete side, and acknowledge that you are
working in a quotient. To shrink the quotient (make more of the source
recoverable from the registry), the next step is adding a `trivia` /
`comments` field to `SymbolEntry`, *not* changing the emitter.

---

## Part 2: AST → Goal-Conditioned Code

### 2.1 Prior Art Survey

**AlphaCode (Li et al., DeepMind 2022)** generates code as token sequences
conditioned on a natural-language problem statement plus example I/O pairs.
It does not reason over an AST internally; the "structure" is implicit in
the transformer's attention. The "goal" is the problem text + test cases.
Selection across thousands of samples is by clustering on *executed
behaviour*, which is the closest goal-conditioning signal AlphaCode uses.

**CodeContests / APPS / HumanEval** are benchmark corpora paired with
test cases; they are the de-facto "goal" representations for modern code-gen
evaluation. The goal in every published system on these benchmarks is a
*test suite* — pass-rate is the reward.

**Sketch (Solar-Lezama, MIT)** and **Rosette (Torlak & Bodik, UW)** are the
canonical *symbolic* program-synthesis frameworks. The goal is encoded as:
(a) a sketch — a partial program with `??` holes; (b) an assertion or
reference implementation to be matched ∀ inputs. Synthesis is by SMT solving,
not learning; the analogy to open-mpm's emit is direct: the registry is
"sketch + assertions", the emitter completes the sketch. The cost is
exponential in the hole count; this is why a learned emitter is interesting.

**PROSE / FlashFill (Microsoft)** uses version-space algebras to learn
programs from input-output examples. The goal is the IO pair set; the search
is symbolic but informed by ranking heuristics that are themselves learned.
This is the *closest published* analogue to "goal-conditioned emit": a
ranking function that picks the best program among a constraint-satisfying
set.

**TF-Coder, Concord, and execution-guided synthesis** (Chen et al., ICLR
2019) use partial-execution feedback to prune the search; the reward is
correctness against examples plus a bias toward shorter programs. Same
shape as the loop one would want for open-mpm: emit, run tests, get reward,
update strategy.

**Code representation learning** for AST-shaped inputs is well-mapped:
**code2vec** (Alon et al. POPL 2019) decomposes the AST into leaf-to-leaf
path contexts and aggregates with attention. **code2seq** (Alon et al.,
ICLR 2019) extends this with LSTM encoder-decoder over paths. **TreeLSTM**
(Tai et al. 2015) and **TBCNN** (Mou et al. 2016) operate over the whole
tree. **CodeBERT / GraphCodeBERT** are transformer-based but still consume
token streams (with optional data-flow edges in GCB). Current frontier
(2024–25) is dominated by decoder-only transformers (StarCoder, DeepSeek-
Coder, Code Llama) that ignore explicit AST structure entirely; the AST
embedding line of work is therefore a *minority* tradition that may have
under-explored upside for structured tasks.

**Goal-conditioned learning** as a framework comes from RL (Schaul 2015,
"Universal Value Function Approximators"). The 2024 NeurIPS paper
"Learning Goal-Conditioned Representations for Language Reward Models"
applies this to LM-based reward modelling — the *reward model* is taught to
predict, at each token, whether the trajectory will reach the desired goal
state. This is the most directly transferable training methodology for
"goal-conditioned emit": train an emit-step value function, decode greedily
or with MCTS against it.

**Gap:** No paper found unifies all three axes — goal-conditioning, AST
structure as input, code as output where the original code is *not* the
target. Existing systems either:
- generate code from spec without an AST in the loop (AlphaCode),
- consume an AST to *predict* something else (code2vec → method names),
- consume an AST to *complete* it under hard constraints (Neo, Sketch),
- generate code from a partial AST/sketch by SMT (Rosette).

The open-mpm hypothesis — emit code from a complete AST under a soft
preference — is a clean novel quadrant.

### 2.2 Formalising the Emit Strategy

Define the emit problem as:

```
emit : (Registry, Goal) → Files
```

where `Goal ∈ G` is a structured spec drawn from a strategy space `G`. The
current open-mpm emitter is `emit_canonical = emit(reg, ⊥)` for a fixed
trivial goal "produce determinism".

Examples of useful `Goal`s:

| Goal class | Encoding | Reward signal |
|------------|----------|---------------|
| **Performance-locality** | "Place callers and callees in the same file when possible; respect a max-file-size budget." | Compile time, runtime cache hit rate, perf benchmarks. |
| **Token-efficiency** (LLM context) | "Minimise total tokens of files an LLM must load to understand symbol X." | `tokens(transitive-closure-of-files-touching(X))`. |
| **Test isolation** | "Co-locate test fixtures with their target; never cross test/non-test layer." | Test discovery time + manual rubric. |
| **Security partitioning** | "Symbols touching credentials live in `crates/auth/`; nothing in there imports user-input parsing." | Static analysis pass + audit. |
| **Readability for newcomers** | "Top of file: types; middle: pure functions; bottom: side-effecting." | Human eval / LLM-as-judge. |
| **Diff-friendliness** | "Minimise expected churn; place volatile symbols last in their file." | Historical diff size against future commits. |

Two formal framings of `Goal`:

1. **Constraint set + lexicographic preference.** `Goal = (C, ≺)` where `C`
   is a set of hard constraints (e.g., "function `foo` must be in
   `auth/oauth.rs`") and `≺` is a preference over satisfying assignments.
   Implementable as ILP / SAT today; solver-bounded.
2. **Reward function.** `Goal = R : Files → ℝ`. Implementable as an RL
   problem; learnable. Hardest piece is making `R` cheap enough to evaluate
   in the inner loop.

These compose: hard constraints define the feasible set, the reward picks
within it. This matches what Sketch/Rosette do at the symbolic level.

### 2.3 Training Signal Options

For a learned emitter, candidate reward signals (from cheapest/noisiest to
most expensive/highest-fidelity):

1. **Static heuristics** — token count, cyclomatic complexity, number of
   cross-file edges. Free, per emit. Floor signal.
2. **Compiler / type-checker pass rate** — does the emitted code compile?
   Cheap (~100ms–10s in Rust); noisy because most emitters that respect
   the registry will compile.
3. **Test suite pass rate** — gold standard for *correctness* preservation.
   The emitter is bounded to produce semantically equivalent code, so this
   should be 100% if the registry truly captures semantics. Useful as a
   *regression* signal: did your emit strategy break something?
4. **LLM-as-judge** for readability / partitioning / naming quality.
   Cheap-ish (one LLM call per emitted file), well-correlated with human
   judgement on style questions.
5. **Profiler / benchmark traces** — for performance-oriented goals.
   Expensive (seconds–minutes per evaluation), but the only signal that
   actually closes the loop on perf claims.
6. **Historical churn** — "would this layout have minimised diffs over the
   last 6 months of git history?" Unique signal: free, and exactly the right
   incentive for diff-friendliness.

For open-mpm specifically, the practical recipe is: gate on (2) + (3) for
correctness, optimise on (1) + (4) for style, save (5) for explicit
performance experiments.

### 2.4 Theoretical Bounds

**Information-theoretic floor.** The mutual information between the original
code and an idealised emitter's output is bounded above by `H(Code) -
H(Code | AST + Goal)`. Anything in the original code that is not in the AST
*and* not derivable from the goal — comments explaining a workaround,
commit messages, the developer's reasoning — is unrecoverable. No learning
fixes this; it has to be added to the inputs.

**Practical implication.** A goal-conditioned emitter that *exceeds* the
quality of the original human-written code is possible only in dimensions
the original code didn't optimise for. For code that was hand-tuned for
readability, a learned emitter can plausibly match but not beat it. For
code that was never tuned for, say, token-efficiency, the same emitter can
beat it on that axis.

**Latent variable view.** Treat the original source as `code = decode(AST,
Z)` for some latent `Z` (formatting choices, comments, variable-naming
rationale). A learned emitter is a model `decode_θ(AST, Goal)` where `Goal`
substitutes for `Z`. The training task is to estimate `Z` from large
corpora, *condition* it on a structured `Goal`, and recover useful values
of the latent without needing the original developer's intent.

**Edge case: when the AST is incomplete.** If the registry has dropped
information that the goal needs (e.g., goal says "minimise diff churn" but
the registry has no edit history), the emitter must either (a) refuse, (b)
fabricate (hallucinate plausible answers), or (c) fall back to the canonical
emit. Refusal is the only safe default; the right product fix is enriching
the registry, not training a smarter emitter.

---

## Part 3: Implications for open-mpm

### 3.1 Current Emitter Architecture

The implementation in `crates/symgraph/src/emitter.rs` (HEAD) is a clean
~260-LOC pure function with the following pipeline:

```
emit(registry, rules) :=
    1. partition: for each (id, entry), file := entry.assigned_file
                  ?? assign_file(id, src_root)        // pure: SymbolId → PathBuf
       result: HashMap<PathBuf, Vec<SymbolId>>
    2. for each file (in sorted PathBuf order):
       a. detect language from extension
       b. split content_ids vs import_ids
       c. order: topological_sort(content_ids, registry)
                 — petgraph::algo::toposort with sorted node insertion
                 — direction: dep → dependent (callees before callers)
       d. render imports: generate_imports(entries, lang) → sorted, deduped
                 + explicit imports from Import-kind entries
       e. concatenate: header banner + imports + entry.source × n
    3. return HashMap<PathBuf, String>
```

`apply_emit(outputs, output_dir)` is the only side-effecting function; it
walks the output map in sorted path order, mkdir-p's parents, and writes
UTF-8.

The `SymbolRegistry` (`registry.rs`) is an `IndexMap<SymbolId, SymbolEntry>`
that re-sorts on every insert. `SymbolEntry` carries: `id`, `kind`,
`source` (the *raw text*), `content_hash` (SHA-256), `language`,
`dependencies: BTreeSet<SymbolId>`, `assigned_file: Option<PathBuf>`,
`test_covers: Option<SymbolId>`.

The `SymbolGraph` (`graph.rs`) is a separate `petgraph::StableGraph<SymbolNode,
EdgeKind>` (Calls / Imports / Contains). Today it is a *view* over the
registry (`SymbolGraph::build_from_registry`) used for caller/callee queries.
It is *not* consulted by `emit()`.

**Key observations:**

- The emitter already separates the three logical concerns: partition
  (`assign_file`), order (`topological_sort`), render (`generate_imports`
  + concat). Each is a candidate `EmitStrategy` extension point.
- `SymbolEntry.source` is a *string*, not an AST. The bijection is therefore
  not at the AST↔code level today — it is at the registry-of-strings ↔
  files level. Drift detection is by SHA-256 (`verify_hashes`). To honour
  Hypothesis 1 strictly the source field would have to be replaced (or
  shadowed) by a structural representation.
- The graph is unused by emit. A goal-conditioned emitter optimising for
  e.g. locality would consume `SymbolGraph` to make grouping decisions.

### 3.2 Making the Emitter Strategy-Pluggable (Concrete API Sketch)

The minimal change that unblocks experimentation:

```rust
// crates/symgraph/src/emit_strategy.rs (new)

use crate::registry::{SymbolEntry, SymbolId, SymbolRegistry};
use crate::graph::SymbolGraph;
use std::collections::HashMap;
use std::path::PathBuf;

/// A pluggable emit strategy. Each method is pure; the trait carries
/// no state of its own (state belongs in `Self`).
pub trait EmitStrategy {
    /// Decide which file each symbol lives in.
    fn partition(
        &self,
        registry: &SymbolRegistry,
        graph: Option<&SymbolGraph>,
    ) -> HashMap<PathBuf, Vec<SymbolId>>;

    /// Order the symbols within a single file.
    fn order(
        &self,
        file: &PathBuf,
        ids: &[SymbolId],
        registry: &SymbolRegistry,
    ) -> Result<Vec<SymbolId>, EmitError>;

    /// Render the entries of one file into source text. Receives the
    /// already-ordered entries plus the language tag.
    fn render(
        &self,
        file: &PathBuf,
        entries: &[&SymbolEntry],
        language: &str,
    ) -> String;

    /// Optional: hard constraints this strategy must respect.
    /// Default: no extra constraints.
    fn validate(&self, _outputs: &HashMap<PathBuf, String>) -> Result<(), EmitError> {
        Ok(())
    }
}

/// Today's `emit` becomes the default implementation.
pub struct CanonicalStrategy {
    pub rules: LayoutRules,
}

impl EmitStrategy for CanonicalStrategy { /* moves current emit logic here */ }

/// Top-level driver — parameterised over strategy.
pub fn emit_with<S: EmitStrategy>(
    registry: &SymbolRegistry,
    graph: Option<&SymbolGraph>,
    strategy: &S,
) -> Result<HashMap<PathBuf, String>> {
    let partitioned = strategy.partition(registry, graph);
    let mut outputs = HashMap::new();
    let mut sorted_files: Vec<&PathBuf> = partitioned.keys().collect();
    sorted_files.sort();
    for file in sorted_files {
        let ordered_ids = strategy.order(file, &partitioned[file], registry)?;
        let entries: Vec<&SymbolEntry> = ordered_ids
            .iter()
            .filter_map(|id| registry.get(id))
            .collect();
        let lang = detect_lang_from_path(file);
        let content = strategy.render(file, &entries, lang);
        outputs.insert(file.clone(), content);
    }
    strategy.validate(&outputs)?;
    Ok(outputs)
}

/// Backwards-compatible alias.
pub fn emit(registry: &SymbolRegistry, rules: &LayoutRules) -> Result<HashMap<PathBuf, String>> {
    emit_with(registry, None, &CanonicalStrategy { rules: rules.clone() })
}
```

Total surface change: one trait, one struct (existing logic relocated), one
new entry point. `apply_emit` is unchanged. All existing call sites keep
their signature via the back-compat `emit` alias.

**Concrete strategies to add next:**

```rust
/// Group strongly-connected components into the same file (locality).
/// Falls back to canonical partitioning for symbols outside any SCC.
pub struct LocalityStrategy {
    pub rules: LayoutRules,
    pub max_file_lines: usize,
}

/// Goal-conditioned strategy with an LLM in the partition step.
/// Prompted with the symbol graph + user-supplied goal text; expected to
/// emit a `HashMap<SymbolId, PathBuf>` plan that the strategy then
/// validates against hard constraints.
pub struct LlmPartitionStrategy {
    pub goal: String,
    pub model: String,
    pub fallback: CanonicalStrategy,
}

/// Performance / token-efficiency / readability scorer wrapped around
/// a candidate strategy; runs the inner strategy, scores the output,
/// keeps the best of N samples.
pub struct BestOfNStrategy<S: EmitStrategy> {
    pub inner: S,
    pub n: usize,
    pub scorer: Box<dyn Fn(&HashMap<PathBuf, String>) -> f64>,
}
```

For training a learned strategy, the API surface stays *exactly the same*:
the model is wrapped in a struct that implements `EmitStrategy`, and
training is offline (collect (registry, goal, files) triples; minimise loss
against a reward function that combines compile/test pass + style judge).

### 3.3 Recommended Next Experiments

In priority order:

1. **Land `EmitStrategy` trait** (~1 day). Cost: refactor only; no behaviour
   change. Benefit: every later experiment is a new struct, not a fork.
2. **Add `trivia: Option<TriviaBlock>` to `SymbolEntry`** (~3 days). Captures
   leading comments, doc strings, blank-line cadence. Shrinks the quotient
   so `parse → registry → emit` is *closer to* a strict bijection on
   well-formed files. Pre-requisite for any "preserve human formatting"
   goal.
3. **Implement two non-canonical hand-written strategies**
   (`LocalityStrategy`, `TestColocationStrategy`) (~1 week). Establishes
   that the abstraction is non-degenerate — i.e. that the strategy actually
   varies meaningful output — before any ML.
4. **Build a deterministic scorer** that takes a `HashMap<PathBuf, String>`
   and returns (compile-ok, tests-pass-rate, token-count, edge-cut). One
   week. Becomes the reward function for any subsequent learning.
5. **LLM-in-the-loop partition strategy** (~1 week). Wraps an LLM call as
   `LlmPartitionStrategy` with hard validation: the LLM may suggest a
   partition; the strategy *only* accepts it if the resulting registry
   slice is internally consistent (no cross-file cycles after
   topo-sorting), else falls back to canonical. This is the cheapest way
   to test the hypothesis without training anything.
6. **Only after 1–5:** explore a learned partition / order policy.
   Conditioned on (registry slice, goal text) → (file plan), trained from
   the (4) reward + (5) demonstrations.

### 3.4 Open Questions

- **What is the right granularity for `Goal`?** Free-text natural language
  (LLM-friendly, informal), structured DSL (machine-checkable, friction
  to author), or a small fixed enum of named goals (cheapest, least
  expressive)? The literature suggests starting with the enum and
  promoting frequently-requested goals to the DSL.
- **Should the emitter consume the `SymbolGraph` directly or rely on
  `SymbolEntry.dependencies`?** Today the registry's `dependencies` is a
  `BTreeSet<SymbolId>` — a strict subset of what the graph encodes (graph
  also has Imports / Contains). For locality strategies the graph is
  richer. Argues for `partition(_, Option<&SymbolGraph>)` as in the
  sketch above.
- **How does the emitter handle conflicts between `assigned_file` (the
  user's manual override) and a goal-conditioned partition?** The current
  `emit` honours `assigned_file` unconditionally. A learned strategy
  must do the same (treat `assigned_file` as a hard constraint), else
  the registry stops being authoritative for layout.
- **Where does the *trivia* live in the AST→code lens?** Two options:
  (a) attached to the `SymbolEntry` it precedes (Rust rust-analyzer model);
  (b) free-floating list on the `RegistryFile` envelope (Roslyn model). The
  former is simpler; the latter handles "comment between two symbols"
  unambiguously.
- **Can the emitter eventually own *naming*?** Variable and parameter
  names are part of `SymbolEntry.source` today, opaque to the emitter.
  A future representation could lift them to the registry as renamable
  metadata, opening up consistency-of-naming as a goal.
- **What is the performance budget?** The current emitter is microseconds
  per file. A learned emitter that calls an LLM per file is seconds per
  file — 6 orders of magnitude. Strategies must therefore be cacheable
  (memoise by `(symbol_subset_hash, goal_hash)`) to be practical at scale.

---

## References

### Bidirectional Transformations and Lenses

- Foster, J. N., Greenwald, M. B., Moore, J. T., Pierce, B. C., Schmitt, A.
  (2007). "Combinators for Bi-Directional Tree Transformations: A
  Linguistic Approach to the View-Update Problem." *ACM TOPLAS* 29(3).
  https://www.cis.upenn.edu/~bcpierce/papers/lenses-toplas-final.pdf
- Bohannon, A., Foster, J. N., Pierce, B. C., Pilkiewicz, A., Schmitt, A.
  (2008). "Boomerang: Resourceful Lenses for String Data." POPL.
  https://www.cis.upenn.edu/~bcpierce/papers/boomerang.pdf
- Foster, J. N., Pilkiewicz, A., Pierce, B. C. (2008). "Quotient Lenses." ICFP.
  https://www.cis.upenn.edu/~bcpierce/papers/quotient-lenses.pdf
- Hofmann, M., Pierce, B. C., Wagner, D. (2011). "Symmetric Lenses." POPL.
- Hofmann, M., Pierce, B. C., Wagner, D. (2012). "Edit Lenses." POPL.
- Bohannon, A., Pierce, B. C., Vaughan, J. A. (2006). "Relational Lenses:
  A Language for Updatable Views." PODS.
- Wikipedia: "Bidirectional transformation."
  https://en.wikipedia.org/wiki/Bidirectional_transformation

### Code Representation Learning (AST-Aware)

- Alon, U., Zilberstein, M., Levy, O., Yahav, E. (2019). "code2vec:
  Learning Distributed Representations of Code." POPL.
  https://arxiv.org/abs/1803.09473
- Alon, U., Brody, S., Levy, O., Yahav, E. (2019). "code2seq: Generating
  Sequences from Structured Representations of Code." ICLR.
- Tai, K. S., Socher, R., Manning, C. (2015). "Improved Semantic
  Representations from Tree-Structured LSTM Networks." ACL.
- Mou, L., Li, G., Zhang, L., Wang, T., Jin, Z. (2016). "Convolutional
  Neural Networks over Tree Structures for Programming Language
  Processing." AAAI.
- Feng, Z. et al. (2020). "CodeBERT: A Pre-Trained Model for Programming
  and Natural Languages." EMNLP.
- Zhang, J., Wang, X., Zhang, H., Sun, H., Wang, K., Liu, X. (2019). "A
  Novel Neural Source Code Representation Based on Abstract Syntax Tree
  (ASTNN)." ICSE. http://hongyujohn.github.io/ASTNN.pdf

### Program Synthesis

- Solar-Lezama, A. (2008). "Program Synthesis by Sketching." PhD thesis,
  UC Berkeley.
- Torlak, E., Bodik, R. (2014). "A Lightweight Symbolic Virtual Machine
  for Solver-Aided Host Languages." PLDI.
  https://emina.github.io/rosette/
- Feng, Y., Martins, R., Bastani, O., Dillig, I. (2018). "Program
  Synthesis using Conflict-Driven Learning." PLDI.
  https://trustml.github.io/docs/pldi18b.pdf
- Gulwani, S. (2011). "Automating String Processing in Spreadsheets Using
  Input-Output Examples." (FlashFill / PROSE foundations.) POPL.
- Chen, X., Liu, C., Song, D. (2019). "Execution-Guided Neural Program
  Synthesis." ICLR.
- Bornholt, J. "Building a Program Synthesizer."
  https://jamesbornholt.com/blog/building-synthesizer/

### Goal-Conditioned and RL-Based Code Generation

- Schaul, T., Horgan, D., Gregor, K., Silver, D. (2015). "Universal Value
  Function Approximators." ICML.
- Li, Y. et al. (DeepMind, 2022). "Competition-Level Code Generation with
  AlphaCode." *Science*.
- "Learning Goal-Conditioned Representations for Language Reward Models."
  NeurIPS 2024.
  https://proceedings.neurips.cc/paper_files/paper/2024/file/d46f127a80dc58cbc0732a717285c43a-Paper-Conference.pdf
- Steccanella, L., Jonsson, A. (2022). "State Representation Learning for
  Goal-Conditioned Reinforcement Learning." ECML PKDD.

### Languages and Tooling Cited

- Rust: `syn`, `quote`, `rust-analyzer` (rowan crate / red-green trees).
- Python: `ast.unparse` (PEP 594), `libcst`.
- Go: `go/ast`, `go/printer`, `gofmt`.
- C: preprocessor semantics; clang's `-fsyntax-only -ast-dump` for
  comparable concrete-syntax-tree access.

### open-mpm Source

- `crates/symgraph/src/emitter.rs` (deterministic registry → file emit,
  ~320 LOC).
- `crates/symgraph/src/registry.rs` (sorted content-addressed registry,
  ~355 LOC).
- `crates/symgraph/src/graph.rs` (petgraph-backed knowledge graph,
  ~548 LOC).
