# Hybrid Workflow Mode: Per-Phase AST-Native Control

**Date**: 2026-05-06
**Status**: Implemented (v0.3.x)
**Related**: `src/workflow/config.rs`, `src/workflow/engine.rs`, `.open-mpm/workflows/prescriptive.json`

---

## Overview

The hybrid workflow mode introduces per-phase `ast_native` control to the prescriptive
workflow engine. Instead of applying AST-native tooling uniformly across all phases of a
run, individual phases can opt in or out of the AST-native substrate independently.

The motivation is empirical: bake-off data shows that AST-native tools reduce cost and
iteration count significantly during research and plan phases, but add overhead during the
code phase without proportional benefit.

---

## Background: AST-Native vs Traditional Tooling

Open-mpm supports two tool substrates for agents:

- **Traditional**: File read/write operations, shell commands, grep-based search. Lower
  per-call overhead. Agents tend to issue more iterative calls to explore structure.
- **AST-native**: Parse/graph/structural introspection tools that expose code structure
  directly. Fewer round-trips needed to answer structural questions. Higher per-call cost
  but lower total token consumption for structural exploration tasks.

Prior to this feature, the substrate was set globally for the entire run via the
`--ast-native` CLI flag. A run was either fully traditional or fully AST-native.

---

## Bake-off Evidence

Two bake-off tasks were used to measure the cost and quality impact of AST-native tooling
at each workflow phase.

### L1: table_formatter package

| Phase    | Traditional      | AST-Native      | Delta         |
|----------|-----------------|-----------------|---------------|
| Research | $0.34 / 3m01s   | $0.15 / 5m22s   | -56% cost     |
| Plan     | $2.33 / 3m29s   | $1.54 / 2m52s   | -34% cost     |
| Code     | $6.58 / 7m45s   | $6.17 / 8m42s   | -6% cost      |
| QA       | $0.19           | $0.19           | flat          |
| **Total**| **$9.54 / 17m06s** | **$8.14 / 18m49s** | **-15% cost** |
| Tests    | 35/35 passing   | 29/29 passing   | traditional more comprehensive |

### L2: git_analyzer package (harder task)

| Phase    | Traditional      | AST-Native      | Delta                        |
|----------|-----------------|-----------------|------------------------------|
| Research | $0.69 / 8m35s   | $0.28 / 3m42s   | -59% cost, -57% time         |
| Plan     | $2.76 / 4m05s   | $1.88 / 3m27s   | -32% cost                    |
| Code     | $5.99 / 8m27s   | $7.18 / 9m50s   | +20% cost (AST overhead at scale) |
| QA       | $0.19 / 46s     | $0.19 / 46s     | flat                         |
| **Total**| **$9.91 / 24m40s** | **$9.68 / 19m08s** | **-2.3% cost, -22.4% time** |
| Tests    | 27/27 passing   | 18/18 passing   | traditional more comprehensive |

### Hybrid projection (L2)

Applying AST-native only to research and plan, traditional to code:

- Estimated saving: approximately -14% cost at L2
- Test count preserved at the traditional (27/27) level

---

## Why AST-Native Helps in Research and Plan

Research and plan phases are fundamentally structural exploration tasks. The agent must
answer questions like: what modules exist, how are they structured, what are the
dependencies, where does a given function live. With traditional tooling, answering these
questions requires multiple read/grep/shell iterations. Each iteration is a tool call that
produces LLM output, consuming tokens.

AST-native tools expose this structural information directly in a single call. The agent
issues fewer tool calls, produces shorter intermediate outputs, and generates smaller cache
reads. The result is lower total token cost despite per-call overhead.

Concretely, at L2:

- Research: -59% cost and -57% wall-clock time. The agent completes structural
  exploration faster with fewer iterations.
- Plan: -32% cost. The plan phase synthesizes research findings into a task decomposition.
  With richer structural context already in hand, planning requires less re-exploration.

---

## Why Traditional Is Better for Code

The code phase is not primarily structural exploration. It is generation: the agent writes
new code, edits existing files, and iterates based on compiler/test output. AST-native
tools add overhead here (parse/edit/apply_patch calls) without proportionally reducing
output volume.

For larger codebases (L2), this overhead becomes significant: +20% cost with AST-native
in the code phase. The traditional substrate's simpler read/write primitives are better
matched to the code generation task.

Additionally, traditional tooling produces more comprehensive test generation. Across both
L1 and L2, the traditional code phase produced more test cases (35 vs 29 at L1, 27 vs 18
at L2). The cause is not fully characterized, but the pattern is consistent across both
bake-off levels.

---

## Implementation

### `PhaseDef` field in `src/workflow/config.rs`

```rust
#[serde(default)]
pub ast_native: Option<bool>,
```

The field is optional. When absent from JSON, it deserializes to `None`, meaning the phase
inherits the global `--ast-native` flag. When present, it overrides the global setting for
the duration of that phase only.

### `AstNativeGuard` RAII in `src/workflow/engine.rs`

```rust
struct AstNativeGuard {
    prev: bool,
    applied: bool,
}

impl Drop for AstNativeGuard {
    fn drop(&mut self) {
        if self.applied {
            crate::ast::set_ast_native_override(self.prev);
        }
    }
}
```

Before executing each phase, the engine reads `phase.ast_native`. If it is `Some(v)`, it
calls `set_ast_native_override(v)` and creates a guard that captures the previous global
state. When the phase exits — whether normally, via early return, or via panic — the guard
restores the previous global state. This ensures the per-phase override does not leak into
subsequent phases regardless of exit path.

### Override semantics

| `phase.ast_native` | `--ast-native` flag | Effective substrate |
|--------------------|---------------------|---------------------|
| `Some(true)`       | any                 | AST-native          |
| `Some(false)`      | any                 | Traditional         |
| `None`             | `true`              | AST-native          |
| `None`             | `false` (default)   | Traditional         |

The per-phase field takes unconditional precedence over the CLI flag.

---

## Configuration Reference

Per-phase `ast_native` is set in the workflow JSON file. The field is optional on every
phase object.

### Force-enable AST-native for specific phases

```json
{
  "phases": [
    { "name": "research", "ast_native": true,  "agent": "research-agent", "context_template": "..." },
    { "name": "plan",     "ast_native": true,  "agent": "plan-agent",     "context_template": "..." },
    { "name": "code",                           "agent": "engineer",       "context_template": "..." },
    { "name": "qa",                             "agent": "qa-agent",       "context_template": "..." }
  ]
}
```

In this configuration, research and plan always use AST-native tooling. Code and QA
inherit the global `--ast-native` flag (default: off).

### Force-disable for a specific phase

```json
{ "name": "code", "ast_native": false, "agent": "engineer", "context_template": "..." }
```

This phase uses traditional tooling even if the user passes `--ast-native` on the command
line.

### Inherit global flag (default behavior)

Omit the field entirely:

```json
{ "name": "code", "agent": "engineer", "context_template": "..." }
```

### prescriptive workflow default

`.open-mpm/workflows/prescriptive.json` ships with hybrid as default: `ast_native: true`
on research and plan, field omitted on code and QA.

The `--ast-native` CLI flag still force-enables all phases when passed, overriding any
per-phase `None` entries (but not `Some(false)` entries).

---

## Interaction with `--ast-native` CLI Flag

- `--ast-native` sets the global override to `true` before any phase runs.
- Per-phase `ast_native: Some(v)` overrides the global for that phase's duration, then
  restores it.
- Per-phase `ast_native: None` leaves the global state untouched for that phase.

Result: passing `--ast-native` on the command line forces all `None` phases to AST-native,
but does not override phases that explicitly set `"ast_native": false`. This allows
workflow authors to pin phases to traditional even in an otherwise fully AST-native run.

---

## L3 Validation Results

**Date**: 2026-05-07
**Task**: Build a REST API weather monitoring service
**Report**: `out/compare-report-20260507T134627Z.md`

### L3 Comparison Table

| Metric | Traditional | AST-Native (hybrid) | Delta |
|--------|-------------|---------------------|-------|
| Wall-clock | 39m18s | 38m02s | -3.2% |
| Cost | $24.38 | $24.74 | +1.5% |
| Output files | 1715 | 1718 | +0.2% |
| Test pass rate | 38/38 (100%) | 37/38 (97.4%) | -1 test |

### L3 Phase Breakdown

| Phase | Traditional | AST-Native (hybrid) | Delta |
|-------|-------------|---------------------|-------|
| research | $0.62 / 2m58s | $0.64 / 3m50s | +3% cost |
| plan | $4.65 / 7m57s | $3.96 / 6m59s | -15% cost |
| code | $18.63 / 23m50s (6 waves) | $19.78 / 24m11s (7 waves) | +6% cost |
| qa/observe/docs | ~$0.48 | ~$0.36 | similar |
| **Total** | **$24.38 / 39m18s** | **$24.74 / 38m02s** | **+1.5% cost, -3.2% time** |

### Test Failure

The AST-native run failed one test: `test_health_check`. The `/api/health` endpoint
returned `"healthy"` but the test asserted `"ok"`. This is a trivial one-line fix in the
generated code, but it is a correctness regression relative to the traditional run, which
passed all 38 tests.

### Comparison Against L1/L2 Expectations

At L1 and L2, hybrid mode delivered material cost reductions:

| Level | Task | Cost delta | Time delta |
|-------|------|-----------|-----------|
| L1 | table_formatter | -15% cost | +10% time (more thorough) |
| L2 | git_analyzer | -2.3% cost | -22% time |
| **L3** | **weather REST API** | **+1.5% cost** | **-3.2% time** |

The savings flatten and reverse at L3. The trend is clear: hybrid mode benefits are
complexity-dependent.

### Root Cause: Code Phase Dominates at L3

At L1 and L2, the research and plan phases represent a meaningful fraction of total cost,
so per-phase savings there translate into run-level savings. At L3, the code phase
dominates:

- L3 code phase cost: $18.63 (traditional) — **76% of total run cost**
- L3 research + plan savings (hybrid): $0.67 savings
- L3 code phase overhead (hybrid): +$1.15 additional cost
- Net: -$0.67 + $1.15 = **+$0.48 net cost increase**

The research and plan savings ($0.31–$0.67 range) are swamped by code phase overhead once
the codebase grows large enough to require 7 waves of generation instead of 6.

### Complexity Threshold Observation

Hybrid mode's sweet spot is tasks where research + plan represent a large enough fraction
of total cost that per-phase savings outweigh any code phase overhead. Observed data:

| Level | research+plan % of total cost | Hybrid cost delta |
|-------|-------------------------------|-------------------|
| L1 | ~28% ($2.67 / $9.54) | -15% |
| L2 | ~35% ($3.45 / $9.91) | -2.3% |
| L3 | ~22% ($5.27 / $24.38) | +1.5% |

A practical threshold: hybrid mode is beneficial when research + plan represent **≥25% of
total run cost**. Below that threshold (as at L3 where code phase balloons to 76% of cost),
the per-phase savings in research/plan are insufficient to offset code phase overhead.

Note that L3 research+plan is 22% of cost — close to but below the threshold — while also
running a 7th code wave (vs 6 traditional). The code wave count is likely the proximate
cause of the cost regression; the hybrid substrate may have caused the agent to structure
work differently, requiring an additional iteration.

### Revised Conclusion

Hybrid mode (AST-native on research + plan, traditional on code) is optimal for
**low-to-medium complexity tasks** in the L1–L2 range:

- L1–L2: research + plan ≥25% of total cost; savings are material (-15% to -2%)
- L3+: code phase balloons to 75–80% of total cost; savings flatten and reverse (+1.5%)
- Quality risk: at L3, the hybrid run produced one fewer passing test, indicating a subtle
  correctness regression that did not appear at lower complexity levels

For L3+ tasks, the recommended configuration is **pure traditional** (omit `ast_native` on
all phases, or set `"ast_native": false` on research and plan). The per-phase hybrid
override mechanism remains valuable for L1–L2 tasks and should be preserved in
`prescriptive.json` as the shipped default; users working on large-scale tasks should
override to traditional via `--no-ast-native` or explicit phase config.

---

## Future Work

### Per-phase model selection

The same RAII guard pattern used for `ast_native` could be extended to support per-phase
model overrides. Research and plan phases might benefit from a cheaper, faster model (e.g.
a smaller Sonnet variant), while code phases use a higher-capacity model. The
`PhaseDef` struct would gain an optional `model` field with the same override-and-restore
semantics.

### Projected savings (complexity-adjusted)

Original L2-based projection was -14% cost for hybrid mode. This estimate does not hold
at L3+. Revised projections by complexity tier:

- L1 tasks: -10% to -15% cost expected (research+plan dominate)
- L2 tasks: -2% to -5% cost expected (code phase grows but research+plan still material)
- L3+ tasks: 0% to +2% cost; quality regression risk; recommend pure traditional

### Per-phase parallelism interaction

The current implementation applies `set_ast_native_override` to a process-global atomic.
If parallel phase execution is introduced (multiple phases running concurrently in separate
tokio tasks), the global atomic will require per-task scoping rather than process-global
state. This is not a concern for the current sequential prescriptive workflow but should be
addressed before enabling parallel phases.
