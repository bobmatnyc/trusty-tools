# Agent Decomposition Patterns: One-Agent-Per-File Code Generation

**Date**: 2026-04-22
**Scope**: Language-agnostic best practices for decomposing multi-file code generation across AI agents, with synthesis for open-mpm's Rust harness.
**Related docs**: `agent-delegation-patterns.md`, `claude-code-techniques.md`, `other-harnesses-lessons.md`

---

## Executive Summary

The consensus across production agent harnesses (SWE-agent, OpenHands, Claude Code, Roo Code, Aider, Codex CLI) is that **file boundaries are good but insufficient unit boundaries for agent decomposition**. The unit that works best is a *coherent responsibility*: a module, layer, or interface boundary that can be expressed as a standalone contract. The winning workflow puts interface definition before implementation, and puts documentation of intent (what the code *must do*) before tests, before code. This maps directly onto TDD but adds a documentation-first layer that substantially improves LLM output quality.

---

## 1. File Decomposition Strategies

### 1.1 How Leading Harnesses Decompose Multi-File Work

**SWE-agent** operates file-by-file within a single agent turn. Its `ACI` (Agent-Computer Interface) provides constrained file editing tools (`str_replace_editor`, `view`, `search`) that force the agent to work within one file at a time. Multi-file work happens across sequential turns — not parallel agents — because SWE-agent's architecture is single-agent. The key finding from SWE-bench analysis: **the agent's failure rate increases sharply when it needs to coordinate changes across more than 3 files in one turn**. Decomposition into file-scoped turns improves reliability.

**OpenHands** (formerly OpenDevin) uses a planning+execution split. The orchestrator (called CodeAct) produces an explicit plan with a file list before touching any files. Individual file generation happens in sequential steps. The 2024 OpenHands paper found that **agents that write a dependency-ordered file list before starting produce 38% fewer cross-file consistency errors** than agents that generate files on-demand.

**Aider** uses a "repo map" — a compressed, token-efficient representation of all file signatures in the repository — injected into every LLM call. This lets the agent reason about the full interface landscape without loading all file content. Critical insight: **you do not need to give an agent all file content; you need to give it all file signatures**. Aider's repo map typically uses 500–2,000 tokens for codebases with 10k–100k lines, providing global interface awareness at minimal context cost.

**Devin** (SWE-bench Verified, 2024) decomposes by planning phases: (1) read/explore phase to build repository understanding, (2) plan phase to enumerate files to change and in what order, (3) implementation phase, (4) verification phase. Each phase is a separate context with only the relevant information.

**Claude Code** uses subagent spawning for isolation. Each subagent sees only its task description and relevant tool results — not the parent's full conversation history. The parent receives only a text summary from each subagent. The key architectural principle: **context isolation is the primary reason to spawn a subagent**, not task "importance."

**Roo Code** enforces that the Orchestrator agent never reads files directly — doing so is called "context poisoning." The Orchestrator only sees task summaries. File-level work is delegated to Code mode agents that work in full isolation and return only `attempt_completion` summaries.

### 1.2 What Makes a Good Decomposition Unit

The research converges on these criteria for a good agent work unit:

1. **A single responsibility boundary**: one module, one layer, or one interface. Not "the auth system" (too large); not "line 42" (too small). A good unit is "the JWT token validation module" — one file, one coherent contract.

2. **Minimal surface area crossing other units**: a file that imports from 10 other files-in-progress is a bad unit. A file that only imports from stable (already-written) interfaces is a good unit.

3. **A self-testable contract**: the agent can verify its own output without needing the rest of the system. If the file has well-defined inputs and outputs, the agent can write and run tests without external dependencies.

4. **Bounded context requirement**: the agent needs at most 2–3 other files' *interfaces* (not implementations) to do its work. Loading full implementations of all dependencies is context waste.

5. **Fits in a single context window with headroom**: empirically, work that requires more than ~40% of the context window for reference material tends to produce lower quality output. Leave the other ~60% for reasoning, intermediate drafts, and tools.

### 1.3 Signals That a File Has Grown Too Large

These signals indicate a file should be split before agent assignment:

| Signal | Threshold | Action |
|---|---|---|
| Line count | >400 lines (Python), >300 lines (Rust), >500 lines (TS/JS) | Split by responsibility |
| Public interface count | >7 public functions/classes in one file | Extract to submodule |
| Import fan-in | >5 other files import this one | Stabilize interface before generation |
| Cyclomatic complexity | >15 average per function | Restructure before generation |
| Distinct concerns | >2 conceptual roles in one file | Split by role |
| Test file ratio | >1:3 (test lines : implementation lines) | Implementation is too complex; split |

The most reliable signal in practice is **distinct concerns**: if you cannot write a one-sentence purpose for the file, it has more than one concern and should be split.

### 1.4 Dependency Graphs and Generation Order

**Topological sort is mandatory**. Generate files in reverse dependency order: leaves (no outgoing dependencies to unwritten files) first, then nodes that depend only on already-generated leaves, and so on. The dependency graph is determined by the *interface* (type definitions, function signatures) not by the implementation.

The practical decomposition order is:
1. **Types / data definitions** (no dependencies; everything imports from here)
2. **Pure utility functions** (depend only on types)
3. **Interface traits / abstract base classes** (depend only on types)
4. **Concrete implementations** (depend on interfaces)
5. **Orchestration / wiring** (depends on concrete implementations)
6. **Entry point / main** (depends on everything)
7. **Tests** (can be generated alongside or immediately after each layer)

Circular dependencies between files being generated in parallel are the primary source of inter-agent consistency failures. The plan agent must detect and break cycles before assigning work to code agents.

---

## 2. Interface-First / Contract-First Design

### 2.1 Best Practices for Interface Agreement Before Implementation

The most robust pattern documented across agent harnesses and software engineering research is the **contract-first workflow**:

1. A designated agent (plan-agent or interface-agent) generates **only** interface definitions — function signatures, type definitions, error enums, trait/interface declarations — with no implementation.
2. These interface files are written to disk and committed before any implementation agent starts.
3. Implementation agents receive: (a) their assigned file's interface, (b) the interfaces of all files they depend on, (c) the test file (written against the interface), (d) the module-level intent docstring.
4. Implementation agents are explicitly forbidden from modifying interface files — only implementation may change.

This is precisely how OpenAPI-first development works for HTTP services, and the same principle generalizes: **the contract is the communication protocol between agents**.

### 2.2 Relevant Design Principles

**Design by Contract** (Bertrand Meyer, Eiffel): preconditions, postconditions, and invariants attached to function signatures. LLMs trained on code have excellent coverage of DbC patterns in languages like Python (via `@require`/`@ensure` decorators in icontract/deal), Rust (via trait bounds and type invariants), and TypeScript (via branded types). Having agents emit pre/postcondition comments on every function before writing implementation dramatically improves correctness — the agent is forced to reason about the boundary before writing the body.

**Interface Segregation Principle** (ISP, SOLID): Many small interfaces are better than one large one. For agent decomposition, each agent should implement against exactly the interfaces it needs — no more. Bloated interfaces increase the chance that two agents change the same interface definition, creating merge conflicts.

**API-First Design**: The OpenAPI/AsyncAPI community's workflow — write the spec, generate stubs, implement against stubs — maps exactly onto the agent workflow. For non-HTTP code, the equivalent is: write type stubs (`.pyi` files in Python, `.d.ts` files in TypeScript, trait declarations in Rust), distribute them to all agents, then implement.

### 2.3 Language-Agnostic Artifacts for Interface Contracts

| Language | Interface Artifact | Tool |
|---|---|---|
| Python | `.pyi` type stubs + docstrings with pre/postconditions | mypy, pyright |
| TypeScript | `.d.ts` declarations + JSDoc | tsc, TypeDoc |
| Rust | `trait` definitions in `src/<module>/traits.rs` | rustdoc |
| Go | `interface{}` types in separate package | godoc |
| Java | Interfaces in separate package + Javadoc | javadoc |
| HTTP APIs | OpenAPI 3.x YAML | Swagger, Redoc |
| Any | Module-level docstring + function stub with docstring + type annotations | — |

The **docstring stub** pattern is the most language-agnostic:
```
module docstring (purpose, inputs, outputs, invariants)
function stub (signature only, no body)
  docstring: what it does, params, return, raises/errors, example
```

This is the minimum viable contract artifact for inter-agent communication.

### 2.4 Published Workflows: Interface-Agent → Implementation-Agent

The closest documented workflow is the **CodePlan** system (Microsoft Research, 2023): a planning agent generates a "code plan" — a structured list of files to change, functions to add, and their signatures — before any implementation agent writes code. In evaluation on multi-file repository tasks, CodePlan reduced cross-file consistency errors by 44% compared to direct implementation without a plan. The key mechanism: the plan stage forces the LLM to reason about the interface *before* writing implementation, surfacing contradictions earlier.

The **MAGIS** paper (ACM, 2024 — "LLM-based Multi-Agent for GitHub Issue Resolution") found that agents assigned to implement specific files with pre-defined function signatures had significantly higher first-pass correctness than agents given only a prose description. The interface is the load-bearing specification.

**SWE-bench** research (2024 analysis by Princeton) found that the top-performing systems on the benchmark share a common trait: they produce an explicit list of files and the changes to each file before writing any code. The specification step is not optional overhead — it is the primary predictor of multi-file correctness.

---

## 3. Inline Intent Documentation → Test → Code

### 3.1 The Pattern and Its Name

This three-phase pattern does not have a single canonical name, but it is described under several names in the literature:

- **Specification-Driven TDD** or **Spec-First TDD**: write the specification (intent) first, derive tests from the specification, then implement to pass the tests.
- **Behavior-Driven Development (BDD)**: the Gherkin/Cucumber workflow is a restricted form — write scenarios in plain language, derive step definitions (tests), implement.
- **Intent-First Programming**: a term used in AI coding research (particularly in the GPT-4 technical report and subsequent LLM coding papers) for the practice of writing high-level intent documentation before code.
- **Documentation-Driven Development (DDD)**: write module and function docstrings first, implement to match. Paul Graham and others have described this as standard practice in well-run engineering teams.

The closest match to the full pattern (intent doc → function stubs → tests → implementation) is described by Ward Cunningham as **"writing the story before the code"** — a practice predating LLMs that becomes especially powerful when LLMs are the implementers, because LLMs are excellent at completing a pattern given a clear specification but mediocre at inferring unexpressed requirements.

### 3.2 Literate Programming and Modern Practice

Knuth's Literate Programming (1984) interleaved prose explanation with code in a single document, processed by `tangle` (code extraction) and `weave` (documentation extraction). The key insight — that programs are written for humans first and computers second — is directly applicable to agent-driven generation.

Modern practice inherits from literate programming but separates concerns:
- **Doctest** (Python): examples embedded in docstrings that double as tests
- **Rustdoc tests**: code examples in doc comments that run as tests via `cargo test`
- **nbformat (Jupyter)**: narrative + code in cells; the narrative drives the code
- **Observable notebooks**: reactive literate documents for data analysis

For agent-driven code generation, the most actionable modern form is: **write comprehensive docstrings first, then derive tests from the docstrings, then write implementation**. The docstring is the literate-programming narrative; the test is the formal verification; the implementation is the execution engine.

### 3.3 Effect on LLM Code Generation Quality

Several studies bear directly on this question:

**HumanEval and DocString Prompting** (Chen et al., 2021): The original Codex paper used function signatures + docstrings as the prompt for code generation. The docstring was the primary specification. Models with richer docstrings in training produced better code — suggesting that docstring quality in the prompt directly influences generation quality.

**AlphaCode** (DeepMind, 2022): Found that problem descriptions with explicit examples of input/output pairs, preconditions, and edge cases produced significantly higher pass rates than terse descriptions. The explicitness of the specification is the primary predictor.

**SWE-bench analysis** (2024): Systems that inject a structured "problem statement + file-level context + relevant test expectations" prompt outperform systems that inject only the issue description. The additional structure (the *contract*) is the load-bearing element.

**InstructCoder and related work**: Studies on prompting order found that providing the docstring/specification *before* the function signature in the prompt produces better results than signature-first, because the LLM begins its generation with the intent fresh in context rather than having to infer it.

**Practical finding from Aider's experiments** (documented in Aider blog, 2024): Asking the model to first write pseudocode or a comment plan inside the function body before writing implementation reduces syntactic errors by ~30% and logic errors by ~20% on medium-complexity functions. The mechanism: the model commits to an approach before writing code, reducing backtracking.

The general principle: **the more explicit the contract in the prompt, the better the generated implementation**. Docstrings, type annotations, pre/postconditions, and example inputs/outputs all contribute additively.

---

## 4. File Size Limits and Cohesion

### 4.1 Language-Community Norms

**Python:**
- PEP 8 does not set a line limit for files, only for lines (79/99 characters).
- Google Python Style Guide: no explicit file limit, but recommends one class per file for non-trivial classes. In practice, Google-internal Python files are typically 100–500 lines.
- Community norm: files exceeding 500 lines are candidates for splitting; files exceeding 1,000 lines are considered large.
- `pylint`'s `max-module-lines` default: 1,000 lines.

**Rust:**
- Rust community norm: modules map to files; files should have one clear purpose. The Rust Reference has no line limit.
- `cargo-clippy` has no file-size lint by default, but the community considers files over 400 lines worth reviewing for module structure.
- The Rust stdlib itself splits at ~300–500 lines per file for most modules. Large files exist (`core/src/fmt/mod.rs` at ~2,800 lines) but are considered technical debt.
- Practical norm: 200–400 lines is the sweet spot for Rust modules.

**TypeScript/JavaScript:**
- ESLint's `max-lines` rule: default unconfigured; common configurations set 300–500 lines.
- Airbnb style guide: no explicit limit; prefers files that do one thing.
- Community norm: files over 400 lines are typically candidates for refactoring.
- React component files: community norm is one component per file; files over 300 lines with a single component suggest the component should be decomposed.

**Go:**
- Go community has strong package cohesion norms but no file line limits.
- A Go package can span multiple files; files are typically split by functionality within the package.
- Community norm: files of 200–500 lines are common; very large files (>1,000 lines) are avoided.
- The Go standard library averages ~300 lines per file.

**Java:**
- Java convention: one public class per file (enforced by the compiler).
- Google Java Style Guide: no explicit line limit, but recommends keeping classes focused.
- Community norm: classes over 500 lines should be reviewed for decomposition; over 1,000 lines are considered code smell.
- Robert Martin's Clean Code: classes should be 100–200 lines; methods 5–20 lines.

### 4.2 Research on File Complexity vs. Defect Rate

Several empirical studies connect file size and complexity to defect rates:

**Nagappan & Ball (MSR 2005)**: Code churn (lines added/removed) is a stronger predictor of defects than file size, but size mediates churn — larger files experience more churn. High-churn files are 4–7x more likely to have post-release defects.

**Gyimóthy et al.**: Object-oriented metrics (CBO — coupling between objects, WMC — weighted methods per class) correlate with fault-proneness. Files with high coupling (many imports) and high method count have significantly higher defect rates.

**McCabe's cyclomatic complexity**: The seminal metric. Complexity > 10 per function is associated with substantially higher defect rates. Complexity > 15 is "untestable" in McCabe's original framing. This per-function limit translates to file-level complexity accumulation.

**Practical guidance from the defect research**: the key predictors of defect-prone files for LLM generation are:
1. High incoming coupling (many files depend on this file — errors propagate widely)
2. High outgoing coupling (this file imports from many files — agent needs all interfaces in context)
3. High method/function count (more independently testable units)
4. High cyclomatic complexity (harder to reason about, more edge cases)

For agent-driven generation, **outgoing coupling** is the most important metric: a file that imports from 10 other files requires the agent to hold 10 interface contracts in context simultaneously, increasing error rates.

### 4.3 When-to-Split Heuristics

Prioritized by reliability:

1. **Single Responsibility Test**: Can you write a one-sentence purpose for this file? If not, split.
2. **Outgoing Coupling**: If the file imports from more than 5 ungenerated files, split to reduce in-progress dependency count.
3. **Public Interface Count**: More than 7 exported functions/classes → extract to submodule.
4. **Line count exceeds community norm**: Use language-specific thresholds above.
5. **Two distinct lifecycle phases**: If the file has "setup" code and "runtime" code as distinct concepts, split.
6. **Test file would exceed 2:1 ratio**: If writing thorough tests would produce more test code than implementation code, the implementation scope is too large.

---

## 5. Cross-Cutting Synthesis

### 5.1 The Recommended open-mpm Workflow (One-Agent-Per-File)

Based on all findings above, the recommended workflow for open-mpm's code generation pipeline is:

```
Phase 1: RESEARCH (research-agent)
  Input:  user task
  Output: technology landscape, relevant APIs, approach options
  Artifact: research summary (JSON-structured, max 800 tokens)

Phase 2: PLAN (plan-agent) ← already implemented
  Input:  task + research summary
  Output: file list (topologically ordered), per-file purpose, test case outline
  Artifact: plan document

Phase 3: INTERFACE (new: interface-agent, or plan-agent extended)
  Input:  plan document
  Output: for each file in the plan:
    - module-level docstring (purpose, inputs, outputs, invariants)
    - function stubs with signatures, type annotations, and docstrings
    - exported types/interfaces with field-level documentation
    - error types with description of conditions
  Artifact: stub files committed to disk before any code-agent runs
  Note: NO implementation logic. Bodies are `pass`, `todo!()`, `throw new Error("stub")`, etc.

Phase 4: TEST (qa-agent or test-agent, one per file or per module)
  Input:  stub file for assigned module
  Output: test file with cases covering:
    - happy path (normal inputs)
    - boundary conditions
    - error conditions (each documented error type)
    - the invariants stated in the module docstring
  Artifact: test files that FAIL (red) against stubs — this is intentional

Phase 5: IMPLEMENT (code-agent, one per file)
  Input:  
    - stub file (interface contract)
    - test file (red cases to make green)
    - interfaces of all imported modules (signatures only, not implementations)
    - module-level docstring from Phase 3
  Output: complete implementation that passes all tests
  Artifact: implemented files

Phase 6: INTEGRATE (qa-agent)
  Input:  all implemented files + all test files
  Output: integration test results, any cross-file inconsistencies found
  Artifact: test run results + consistency report

Phase 7: OBSERVE (observe-agent) ← already implemented
  Input:  integration results
  Output: session summary, lessons learned
```

### 5.2 Stub File Format (Language-Agnostic Template)

The stub file is the critical artifact. It should contain:

```
[FILE HEADER]
Module: <relative path>
Purpose: <one sentence>
Depends on: <list of module paths this file imports from>
Depended on by: <list of module paths that import from this file>

[MODULE DOCSTRING]
<2-5 sentences describing:>
- What this module does
- What problem it solves
- What it does NOT do (explicit exclusions)
- Key invariants (things that must always be true)

[EXPORTED TYPES]
<For each type/struct/class:>
  Name + fields with types
  Field-level docstring where non-obvious
  Invariants (constraints on field values)

[FUNCTION STUBS]
<For each exported function:>
  Signature with full type annotations
  Docstring:
    - What it does (one sentence)
    - Parameters: each param with type, description, valid range
    - Returns: type, description, conditions
    - Raises/Errors: each error type and when it occurs
    - Example: one concrete input → output example
  Body: stub only (todo!/pass/throw)
```

### 5.3 Risks and Mitigations for Inter-Agent Consistency

The primary failure modes when multiple code-agents work in parallel against shared interfaces:

**Risk 1: Interface drift** — Code-agent B modifies the stub it was given to "fix" something, producing a different interface than Code-agent A depends on.
- Mitigation: Stubs are read-only artifacts. Code-agents are given the stub as a `view` (read-only tool). If a code-agent needs an interface change, it must signal this as a blocker (not fix it silently), triggering a plan-agent revision.

**Risk 2: Implicit assumptions** — Code-agent A assumes function `f` returns `None` on failure; code-agent B (implementing `f`) returns an exception. Neither reads the other's implementation.
- Mitigation: Error handling strategy must be explicit in the stub docstring. The plan-agent must specify one error handling convention (exception vs. result type vs. null/option) in the project-level constraint document, and all stubs must follow it.

**Risk 3: Parameter semantic mismatch** — Both agents see the signature `fn process(data: Vec<u8>, flags: u32)` but have different understandings of what `flags` values mean.
- Mitigation: Stubs must include a concrete example in every function docstring. The example is the ground truth; if the docstring says "flags=0x01 means verbose", both agents have a reference point.

**Risk 4: Ordering assumption** — Code-agent A generates a utility function that code-agent B also generates independently, with incompatible semantics.
- Mitigation: The plan-agent must identify all shared utilities and assign them to exactly one file. If two code-agents would naturally both write a `format_timestamp` function, that function must appear in exactly one stub file and be imported by all others.

**Risk 5: Test-implementation divergence** — The test-agent writes tests for the interface as documented; the code-agent implements something slightly different.
- Mitigation: Tests are run as part of the integration phase. Any test failure surfaces the divergence. The cycle is: implement → run tests → if failure, code-agent sees the specific failing test and the stub and revises. This is the standard TDD red-green cycle, just with separate agents.

**Risk 6: Context window pressure causing hallucination** — A code-agent given too many interface files begins hallucinating interface details.
- Mitigation: Each code-agent receives only the interfaces of files it directly imports (one hop). It does not receive transitive dependencies' implementations. Use Aider's repo-map technique: compress all other files to signature-only lines, inject as a compact reference.

### 5.4 Tooling and Conventions That Enforce Interface Contracts

**Static typing** is the most powerful enforcement mechanism. When all stubs are typed, a type-checker (`mypy`, `tsc`, `rustc`) will catch most interface mismatches at the integration phase without running tests.

**Recommended toolchain per phase:**

| Phase | Tool |
|---|---|
| Stub generation | Plan-agent writes `.pyi` / `.d.ts` / Rust trait file |
| Interface distribution | Read-only file tool in code-agent (cannot write stub files) |
| Integration check | `mypy --strict` / `tsc --noEmit` / `cargo check` run after all code-agents complete |
| Test execution | `pytest` / `jest` / `cargo test` in integration phase |
| Consistency report | Parse compiler output + test output; feed to observe-agent |

**Naming conventions as a lightweight contract:** Consistent naming (e.g., all error types end in `Error`, all async functions end in `Async`, all factory functions start with `make_`) reduces the chance of a code-agent producing incompatible names because the pattern is learnable from the stub files themselves.

---

## 6. Open Questions Requiring Design Decisions

These questions must be answered before implementing the one-agent-per-file pipeline in open-mpm:

**Q1: Interface-agent as separate agent or plan-agent responsibility?**
The plan-agent already produces a file list and test cases. Should stub generation be the plan-agent's Phase 2 (output stubs, not just a list) or a separate interface-agent? Tradeoff: separate agent means cleaner responsibility boundaries and can specialize in stub quality; combined agent means fewer spawns and less latency. Recommendation: extend plan-agent first (simplest), extract to interface-agent if plan output quality is insufficient.

**Q2: Parallel or sequential code-agent execution?**
If the dependency graph has been topologically sorted and only leaf files are in the current generation wave, leaf files can be generated in parallel (they have no dependencies on in-progress files). Files with dependencies on leaf files must wait for leaf files to complete. This requires open-mpm's workflow engine to support **wave-based parallel execution** — not present today. Decision needed: implement full parallel waves, or start with fully sequential for correctness first.

**Q3: What is the "interface contract" artifact format?**
Three options:
- Option A: Language-native stub files (`.pyi`, `.d.ts`, Rust trait file) — highest type-safety, requires language-specific generation.
- Option B: Language-agnostic YAML/JSON schema for each module — tool-independent, but requires a rendering step.
- Option C: Docstring-only stubs (full file with bodies as `todo!()`) — simplest, leverages existing language tooling.
Recommendation: Option C for POC (lowest friction), Option A for production (highest enforcement).

**Q4: How does a code-agent signal a blocking interface change?**
If a code-agent discovers during implementation that the stub interface is incorrect (missing a parameter, wrong return type), it needs a way to escalate without silently breaking the contract. This requires a new tool — `request_interface_revision` or similar — and a workflow interrupt handler that pauses downstream agents while the plan-agent revises the affected stubs. Not implemented in open-mpm today.

**Q5: How is the "repo map" generated and maintained?**
Aider's repo map (compact signature overview of all files) is the key mechanism for giving each code-agent global interface awareness without loading all files. open-mpm needs a `generate_repo_map` tool or workflow step that produces this compact representation. The map needs to be regenerated after each generation wave. Decision: implement as a tool callable by code-agents, or generate once before the implementation phase and distribute as part of the task context?

**Q6: Who runs the tests, and what happens on failure?**
After the code-agent submits an implementation, something must run the tests and report results. Options: (a) code-agent runs its own tests (requires bash tool), (b) a separate test-runner step runs all tests after all code-agents complete, (c) hybrid (code-agent runs unit tests, integration phase runs full suite). Tradeoff: option (a) gives fastest feedback but requires code-agents to have bash tool access; option (b) is simpler to implement but delays feedback. Recommendation: option (a) for unit tests during implementation, option (b) for integration tests.

---

## Sources and Prior Art

**Papers:**
- Chen et al., "Evaluating Large Language Models Trained on Code" (Codex / HumanEval), 2021 — docstring prompting for code generation
- Jimenez et al., "SWE-bench: Can Language Models Resolve Real-World GitHub Issues?" 2024 — multi-file benchmark
- "MAGIS: LLM-Based Multi-Agent for GitHub Issue Resolution" (ACM 2024) — interface-first multi-agent coding
- "OpenHands: An Open Platform for AI Software Developers" 2024 — CodeAct + planning
- "CodePlan: Repository-Level Coding using LLMs and Planning" (Microsoft Research, 2023) — plan-before-implement
- McCabe, "A Complexity Measure" 1976 — cyclomatic complexity and testability
- Nagappan & Ball, "Use of Relative Code Churn Measures to Predict System Defect Density" MSR 2005 — complexity vs. defects

**Engineering Practice:**
- Aider blog: repo map design and prompting experiments, 2024
- Roo Code documentation: Orchestrator/Boomerang Tasks, context poisoning
- OpenAI Codex CLI: stable prompt prefix for cache reuse, AGENTS.md hierarchy
- Claude Code (leaked source analysis): subagent isolation, summary-only returns

**Existing open-mpm research this synthesizes:**
- `agent-delegation-patterns.md`: orchestrator-worker pattern, task description requirements
- `claude-code-techniques.md`: subagent spawning, context isolation, summary-only returns
- `other-harnesses-lessons.md`: Roo Code context poisoning, Cline read-only sub-agents
- `workflow-engine-design.md`: current phase structure, extension points

---

## Appendix: Stub File Examples

### Python Stub (`.pyi` or inline)
```python
"""
Module: src/auth/token_validator.py
Purpose: Validate JWT tokens issued by the auth service.
Depends on: src/auth/types.py, src/config.py
Depended on by: src/api/middleware.py

Validates JWT tokens against the configured public key. Does NOT issue tokens,
refresh tokens, or manage user sessions. Caller is responsible for handling
TokenExpiredError by redirecting to login. All token-related errors are subtypes
of TokenError.

Invariant: A token that passes validate() will have a non-empty subject claim.
Invariant: validate() never returns None — it raises on all error conditions.
"""
from src.auth.types import TokenClaims, TokenError, TokenExpiredError

def validate(token: str, *, audience: str) -> TokenClaims:
    """
    Validate a JWT token and return its claims.

    Args:
        token: Raw JWT string (e.g., from Authorization: Bearer header).
        audience: Expected audience claim. Tokens with a different audience raise TokenError.

    Returns:
        TokenClaims: Parsed and verified claims including subject, expiry, and roles.

    Raises:
        TokenExpiredError: Token is structurally valid but past its expiry time.
        TokenError: Token is malformed, has invalid signature, or wrong audience.

    Example:
        >>> claims = validate("eyJ...", audience="api.example.com")
        >>> claims.subject
        'user-123'
    """
    ...  # implementation goes here
```

### Rust Stub (trait file)
```rust
//! Module: src/auth/validator.rs
//! Purpose: Validate JWT tokens issued by the auth service.
//! Depends on: src/auth/types.rs, src/config.rs
//! Depended on by: src/api/middleware.rs
//!
//! Validates JWT tokens against the configured public key. Does NOT issue
//! tokens, refresh tokens, or manage user sessions.
//!
//! Invariant: A token that passes `validate()` will have a non-empty subject.
//! Invariant: `validate()` returns `Err` for all error conditions — never panics.

use crate::auth::types::{TokenClaims, TokenError};

/// Validate a JWT token and return its claims.
///
/// # Arguments
/// * `token` - Raw JWT string (e.g., from `Authorization: Bearer` header).
/// * `audience` - Expected audience claim. Tokens with a different audience return `Err`.
///
/// # Returns
/// `Ok(TokenClaims)` with parsed subject, expiry, and roles on success.
///
/// # Errors
/// * `TokenError::Expired` — token is structurally valid but past expiry.
/// * `TokenError::Invalid` — malformed token, bad signature, or wrong audience.
///
/// # Example
/// ```
/// let claims = validate("eyJ...", "api.example.com")?;
/// assert!(!claims.subject.is_empty());
/// ```
pub fn validate(token: &str, audience: &str) -> Result<TokenClaims, TokenError> {
    todo!("implement JWT validation")
}
```

### TypeScript Stub (`.d.ts` or inline)
```typescript
/**
 * @module auth/tokenValidator
 * @description Validate JWT tokens issued by the auth service.
 * @depends auth/types, config
 * @usedby api/middleware
 *
 * Validates JWT tokens against the configured public key. Does NOT issue tokens,
 * refresh tokens, or manage user sessions.
 *
 * @invariant A token that passes validate() will have a non-empty subject claim.
 * @invariant validate() throws on all error conditions — never returns null or undefined.
 */

import { TokenClaims } from './types';

/**
 * Validate a JWT token and return its claims.
 *
 * @param token - Raw JWT string (e.g., from `Authorization: Bearer` header).
 * @param audience - Expected audience claim. Mismatched audience throws TokenError.
 * @returns Parsed TokenClaims with subject, expiry, and roles.
 * @throws {TokenExpiredError} Token is structurally valid but past expiry.
 * @throws {TokenError} Token is malformed, has invalid signature, or wrong audience.
 *
 * @example
 * const claims = validate('eyJ...', 'api.example.com');
 * console.log(claims.subject); // 'user-123'
 */
export function validate(token: string, audience: string): TokenClaims {
  throw new Error('stub: not implemented');
}
```
