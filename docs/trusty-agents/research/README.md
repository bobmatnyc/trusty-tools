# open-mpm — Research Index

Investigation, audit, design, and bug-analysis documents that informed open-mpm.
These are **dated, point-in-time** records preserved as-is; for the current
authoritative design, see [`../spec/`](../spec/). Files are grouped by theme
below; every file in this directory is linked.

## Foundations & frameworks

| File | Topic |
|---|---|
| [rust-ai-frameworks.md](./rust-ai-frameworks.md) | Existing Rust AI frameworks: Rig, Swiftide, llm-chain, AutoAgents, rmcp |
| [rust-ecosystem-utilities.md](./rust-ecosystem-utilities.md) | Useful Rust ecosystem crates and utilities |
| [restate-evaluation.md](./restate-evaluation.md) | Restate durable-execution engine evaluation (No-Go verdict) |
| [tantivy-surrealdb-memory-evaluation.md](./tantivy-surrealdb-memory-evaluation.md) | Tantivy + SurrealDB memory-store evaluation |
| [tokio-bounded-task-queue.md](./tokio-bounded-task-queue.md) | Bounded task-queue design with tokio |

## IPC, process & event model

| File | Topic |
|---|---|
| [subprocess-ipc-patterns.md](./subprocess-ipc-patterns.md) | NDJSON IPC design, deadlock prevention |
| [process-model-and-event-architecture.md](./process-model-and-event-architecture.md) | Process model and event-bus architecture |
| [global-infra-architecture.md](./global-infra-architecture.md) | Global / cross-project infrastructure architecture |

## Dispatch, delegation & LLM backends

| File | Topic |
|---|---|
| [agent-delegation-patterns.md](./agent-delegation-patterns.md) | Orchestrator-worker, agents-as-tools, handoffs |
| [agent-decomposition-patterns.md](./agent-decomposition-patterns.md) | One-agent-per-file decomposition, TDD order |
| [llm-dispatch-pathway-analysis.md](./llm-dispatch-pathway-analysis.md) | LLM dispatch pathway analysis |
| [openrouter-api.md](./openrouter-api.md) | OpenRouter API configuration for async-openai |
| [openai-plans.md](./openai-plans.md) | OpenAI reasoning models, preambles, plan mode |
| [opus-tool-calling-reliability.md](./opus-tool-calling-reliability.md) | Tool-calling reliability with Opus models |
| [provider-model-slash-commands-design.md](./provider-model-slash-commands-design.md) | Provider/model slash-command design |

## Harness lessons & techniques

| File | Topic |
|---|---|
| [claude-code-techniques.md](./claude-code-techniques.md) | Claude Code internal architecture: agentic loop, tool schemas |
| [claude-code-harness-lessons.md](./claude-code-harness-lessons.md) | Lessons from the Claude Code harness |
| [claude-code-prompt-patterns.md](./claude-code-prompt-patterns.md) | Claude Code prompt patterns |
| [codex-techniques.md](./codex-techniques.md) | Codex CLI techniques |
| [kilo-ai-analysis.md](./kilo-ai-analysis.md) | Kilo AI analysis |
| [other-harnesses-lessons.md](./other-harnesses-lessons.md) | Roo Code, Cline, Codex: context condensation, shadow Git |
| [coding-personas-and-idiomatic-skill-packs.md](./coding-personas-and-idiomatic-skill-packs.md) | Coding personas and idiomatic skill packs |

## Skills system

| File | Topic |
|---|---|
| [skill-system-gap-analysis.md](./skill-system-gap-analysis.md) | Skill system gap analysis |
| [skill-selection-nlp-migration.md](./skill-selection-nlp-migration.md) | NLP-based skill-selection migration |
| [skills-system-sources.md](./skills-system-sources.md) | Sources for the skills system |

## Workflow engine

| File | Topic |
|---|---|
| [workflow-engine-design.md](./workflow-engine-design.md) | Workflow engine design: phases, tools |
| [hybrid-workflow-mode.md](./hybrid-workflow-mode.md) | Hybrid workflow mode |

## Token & prompt compression

| File | Topic |
|---|---|
| [output-token-optimization.md](./output-token-optimization.md) | Output-token optimization strategies |
| [prompt-compression-nlp.md](./prompt-compression-nlp.md) | NLP-based prompt compression |
| [token-compression-rtk-ztk.md](./token-compression-rtk-ztk.md) | RTK/ZTK token-compression approaches |
| [token-tracking-infrastructure-2026-05.md](./token-tracking-infrastructure-2026-05.md) | Token-tracking infrastructure |

## AST & code analysis

| File | Topic |
|---|---|
| [declarative-ast-code-theory.md](./declarative-ast-code-theory.md) | Declarative AST code theory |
| [ast-multilang-eval.md](./ast-multilang-eval.md) | Multi-language AST evaluation |
| [java-ast-native-regression.md](./java-ast-native-regression.md) | Java AST native regression |
| [multi-repo-quality-analysis.md](./multi-repo-quality-analysis.md) | Multi-repo quality analysis |
| [codebase-areas-pre-feature-analysis.md](./codebase-areas-pre-feature-analysis.md) | Pre-feature codebase-area analysis |

## UI / TUI / REPL surfaces

| File | Topic |
|---|---|
| [ui-surface-inventory-2026-05.md](./ui-surface-inventory-2026-05.md) | UI surface inventory |
| [ui-surface-spec-2026-05.md](./ui-surface-spec-2026-05.md) | UI surface specification |
| [repl-ui-evaluation.md](./repl-ui-evaluation.md) | REPL UI evaluation |
| [repl-tui-library-evaluation-2026-05.md](./repl-tui-library-evaluation-2026-05.md) | REPL/TUI library evaluation |
| [repl-internals-debugger-design.md](./repl-internals-debugger-design.md) | REPL internals & debugger design |
| [tui-key-handling-popup-selector.md](./tui-key-handling-popup-selector.md) | TUI key handling & popup selector |
| [tui-issues-scroll-detection-statusline-2026-05.md](./tui-issues-scroll-detection-statusline-2026-05.md) | TUI scroll/statusline issues |
| [slash-command-autocomplete-design-2026-05.md](./slash-command-autocomplete-design-2026-05.md) | Slash-command autocomplete design |
| [tauri-chat-interface-design.md](./tauri-chat-interface-design.md) | Tauri chat-interface design |

## Clients & integrations

| File | Topic |
|---|---|
| [ai-commander-telegram-client.md](./ai-commander-telegram-client.md) | AI Commander Telegram client |
| [ai-commander-tmux-client.md](./ai-commander-tmux-client.md) | AI Commander tmux client |
| [tm-tmux-manager-spec.md](./tm-tmux-manager-spec.md) | tmux-manager specification |
| [tm-promptable-architecture-2026-05.md](./tm-promptable-architecture-2026-05.md) | Promptable architecture |
| [cto-bot-port-analysis.md](./cto-bot-port-analysis.md) | CTO-bot port analysis |
| [openrpc-trusty-contract.md](./openrpc-trusty-contract.md) | OpenRPC trusty contract (wire format) |
| [projects-feature-foundation.md](./projects-feature-foundation.md) | Projects-feature foundation |
| [service-layer-audit.md](./service-layer-audit.md) | Service-layer audit |

## Bake-off & evaluation

| File | Topic |
|---|---|
| [bake-off-challenges.md](./bake-off-challenges.md) | Bake-off test-case definitions |
| [bakeoff-v016-analysis.md](./bakeoff-v016-analysis.md) | Bake-off v0.16 analysis |
| [bakeoff-analysis-v0118.md](./bakeoff-analysis-v0118.md) | Bake-off v0.118 analysis |
| [bakeoff-rubric-gap-analysis.md](./bakeoff-rubric-gap-analysis.md) | Bake-off rubric gap analysis |

## Audits, test infrastructure & bug analyses

| File | Topic |
|---|---|
| [crate-audit-2026-04.md](./crate-audit-2026-04.md) | Crate audit (2026-04) |
| [test-infrastructure-survey-2026-04-26.md](./test-infrastructure-survey-2026-04-26.md) | Test-infrastructure survey |
| [test-analysis-gap-analysis-2026-04-30.md](./test-analysis-gap-analysis-2026-04-30.md) | Test-analysis gap analysis |
| [cli-test-harness-gap-analysis.md](./cli-test-harness-gap-analysis.md) | CLI test-harness gap analysis |
| [ctrl-latency-analysis-2026-05.md](./ctrl-latency-analysis-2026-05.md) | CTRL latency analysis |
| [bug-pm-thinking-ctrl-slowness-2026-05.md](./bug-pm-thinking-ctrl-slowness-2026-05.md) | PM-thinking / CTRL slowness analysis |
| [bug-1-workflow-exit-1-analysis.md](./bug-1-workflow-exit-1-analysis.md) | Workflow exit-1 bug analysis |
| [bug-212-api-server-restarts.md](./bug-212-api-server-restarts.md) | Bug #212: API server restarts |
| [bug-analysis-token-tracking-skill-selection.md](./bug-analysis-token-tracking-skill-selection.md) | Token-tracking / skill-selection bug analysis |
| [bugs-207-208-209-analysis.md](./bugs-207-208-209-analysis.md) | Bugs #207/#208/#209 analysis |
| [failing-tests-210-analysis.md](./failing-tests-210-analysis.md) | Failing-tests #210 analysis |
| [telegram-bot-unresponsiveness-2026-05.md](./telegram-bot-unresponsiveness-2026-05.md) | Telegram-bot unresponsiveness analysis |

---

[← Back to open-mpm docs index](../README.md)
