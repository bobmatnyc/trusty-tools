# Skill Catalog Reference

Generated from `bundle::ALL` (filtered to top-level `skills/*.md` entries) + a shared frontmatter line parser. Regenerate with `tm generate capabilities`.

51 bundled skills.

| Skill | Category | User-invocable | Description |
|---|---|---|---|
| `api-design-patterns` | agent-reference | no | Comprehensive API design patterns covering REST, GraphQL, gRPC, versioning, authentication, and modern API best practices |
| `api-documentation` | agent-reference | no | Best practices for documenting APIs and code interfaces, eliminating redundant documentation guidance per agent. |
| `artifacts-builder` | agent-reference | no | Suite of tools for creating elaborate, multi-component claude.ai HTML artifacts using modern frontend web technologies (React, Tailwind CSS, shadcn/ui) |
| `brainstorming` | agent-reference | no | Interactive idea refinement using Socratic method to develop fully-formed designs |
| `code-production-process` | agent-reference | no | Six-stage quality-gate pipeline for any code implementation task |
| `code-review-standards` | agent-reference | no | Adversarial code review rubric — severity taxonomy, the 80% confidence filter, and the APPROVE/WARN/BLOCK verdict protocol. Loaded by the code-critic agent as its primary review reference. |
| `condition-based-waiting` | agent-reference | no | Replace arbitrary timeouts with condition polling for reliable async tests |
| `contract-driven-testing` | agent-reference | no | Derive a three-level test pyramid directly from a function's Code Contracts (preconditions, postconditions, invariants) — contract-targeted unit tests, property-based tests, precondition-violation tests, plus the no-contracts fallback and the contract review checklist. Loaded by the code-critic agent. |
| `database-migration` | agent-reference | no | Safe patterns for evolving database schemas in production with decision trees and troubleshooting guidance. |
| `documentation-style` | agent-reference | no | SLD-grounded per-artifact-type documentation style guide: the four-axis Why/What/Test + Spec-References inline model, plus focused conventions for specs, READMEs, files, classes, methods/functions, and block comments. Use when writing or reviewing any documentation artifact. |
| `env-manager` | agent-reference | no | Environment variable validation, security scanning, and management for Next.js, Vite, React, and Node.js applications |
| `git-workflow` | agent-reference | no | Essential Git patterns for effective version control, eliminating redundant Git guidance per agent. |
| `internal-comms` | agent-reference | no | Framework for writing concise 3P (Progress, Plans, Problems) team updates for executives and stakeholders |
| `json-data-handling` | agent-reference | no | Working effectively with JSON data structures. |
| `model-context-builder` | agent-reference | no | MCP (Model Context Protocol) server build and evaluation guide, including local conventions for tool surfaces, config, and testing |
| `requesting-code-review` | agent-reference | no | Dispatch code-reviewer subagent to review implementation against plan or requirements before proceeding |
| `root-cause-tracing` | agent-reference | no | Systematically trace bugs backward through call stack to find original trigger |
| `rust-build-performance` | agent-reference | no | Practical Rust build-performance discipline for the inner dev loop: cargo check first, measure with --timings before tuning, trim the dependency/feature graph, preserve incremental compilation, and use sccache across worktrees. Use when a Rust build feels slow or before reaching for compiler-flag tricks. |
| `security-scanning` | agent-reference | no | CI security scanning: secrets, deps, SAST, triage, expiring exceptions |
| `software-patterns` | agent-reference | no | Compare tradeoffs and recommend architectural patterns — dependency injection, service-oriented architecture, repository, domain events, circuit breaker, and anti-corruption layer. Use when choosing between design patterns, planning microservices boundaries, evaluating system design alternatives, or asking 'which pattern should I use' for a specific coupling or resilience problem. |
| `systematic-debugging` | agent-reference | no | Step-by-step debugging workflow: reproduce the bug, isolate the failing component, trace to root cause, apply a targeted fix, and verify the fix resolves the issue without regressions. Use when you encounter a bug, error, exception, crash, or unexpected behavior that needs troubleshooting. |
| `test-driven-development` | agent-reference | no | Comprehensive TDD patterns and practices for all programming languages, eliminating redundant testing guidance per agent. |
| `test-quality-inspector` | agent-reference | no | Test quality inspection framework for reviewing test coverage, identifying gaps, and ensuring comprehensive validation |
| `testing-anti-patterns` | agent-reference | no | Never test mock behavior. Never add test-only methods to production classes. Understand dependencies before mocking. Language-agnostic principles with TypeScript/Jest and Python/pytest examples. |
| `tm` | pm-reference | yes | trusty-mpm orchestration model overview — agents, skills, delegation |
| `tm-adr` | pm-optional | yes | Architecture Decision Records (ADRs) — opt-in convention for documenting significant, hard-to-reverse architectural decisions using the Nygard template |
| `tm-agent-architecture` | pm-workflow | no | Official vs custom agent workflow — how to safely update trusty-mpm's compose-chain agent catalog |
| `tm-bug-reporting` | pm-workflow | no | Bug reporting protocol for the PM and agents — routes through the MCP-native list_recent_errors / preview_bug_report / report_bug pipeline |
| `tm-capabilities` | pm-reference | no | Auto-generated exhaustive harness capability catalog — every tm CLI command, MCP tool, bundled agent, bundled skill, and doctor check, verbatim and always current. Complements (does not replace) the conceptual `tm` skill. |
| `tm-circuit-breaker` | pm-framework | no | Complete circuit breaker enforcement patterns with examples and remediation for the trusty-mpm PM |
| `tm-cli-operations` | pm-reference | yes | Operate the tm / trusty-mpm CLI — set up and manage MCP servers, drive session lifecycle, and run health diagnostics |
| `tm-delegation-patterns` | pm-reference | no | Delegation matrices and agent-selection decision trees for the trusty-mpm PM |
| `tm-doctor` |  | no | Run a full trusty-mpm system diagnostic checking instructions, agents, skills, memory, and search services |
| `tm-git-file-tracking` | pm-workflow | no | Protocol for tracking files immediately after agent creation, before marking work complete |
| `tm-init` | pm-workflow | yes | Initialize or intelligently refresh a project for trusty-mpm — analyze the repo and scaffold or update CLAUDE.md (project instructions), register the project with the daemon, and offer update/context/catchup modes |
| `tm-issues-prune` | pm-workflow | yes | Prune, organize, prioritize, and suggest next tasks from a project's GitHub issue backlog — natural-language PM delegation pattern (gh-first, JIRA deferred) |
| `tm-postmortem` | pm-workflow | yes | Analyze session errors captured across trusty-* daemons and route them through the bug-reporting pipeline |
| `tm-pr-workflow` | pm-workflow | no | Branch protection, trusty-review gate, squash-merge, and worktree discipline for landing work on main |
| `tm-session-management` | pm-workflow | yes | PM context-limit pause/resume, project-local session snapshots, worktree pruning, and task-list integration |
| `tm-session-pause` | pm-workflow | yes | Pause the current PM session — snapshot todos, git state, and context to a project-local session file, prune stale worktrees, and print the resume path |
| `tm-session-resume` | pm-workflow | yes | Resume from a paused PM session — scan project-local snapshots, validate the project matches, load the latest (or a selected) session, and restore todos and context |
| `tm-teaching-templates` | pm-workflow | no | Progressive-disclosure teaching templates for onboarding users to trusty-mpm concepts |
| `tm-ticketing` | pm-workflow | yes | Ticket-driven development protocol and high-level ticketing orchestration for the trusty-mpm PM |
| `tm-tool-usage-guide` | pm-reference | no | Detailed tool usage patterns and examples for the trusty-mpm PM agent |
| `tm-verification-protocols` | pm-workflow | no | QA verification gate and evidence requirements for the trusty-mpm PM |
| `tm-workflow` | pm-workflow | yes | Manage and customize the trusty-mpm PM workflow via project-level .trusty-mpm/ override files |
| `verification-before-completion` | agent-reference | no | Run verification commands and confirm output before claiming success |
| `web-performance-optimization` | agent-reference | no | Optimize web performance using Core Web Vitals, modern patterns (View Transitions, Speculation Rules), and framework-specific techniques |
| `webapp-testing` | agent-reference | no | Comprehensive web application testing patterns with Playwright selectors, wait strategies, and best practices |
| `writing-plans` | agent-reference | no | Create detailed implementation plans with bite-sized tasks for engineers with zero codebase context |
| `xlsx` | agent-reference | no | Working with Excel files programmatically. |
