# Agent Delegation for trusty-tools

## Primary Agents

| Agent | Use When |
|-------|----------|
| rust-engineer (opus) | All Rust implementation, refactoring, new crates |
| research (sonnet) | Codebase investigation, architecture analysis |
| local-ops (sonnet) | cargo publish, launchd, version bumps, CI |
| version-control (haiku) | PRs, branches, git operations |
| ticketing (haiku) | GitHub Issues CRUD |
| qa (sonnet) | Test verification, clippy, fmt checks |

## Rust Engineer is the Primary Agent
Most work in this repo goes to rust-engineer. It should:
1. Research the codebase if needed
2. Implement the change
3. Run cargo test --workspace
4. Run cargo clippy
5. Run cargo fmt
6. Report results with raw output

## Alias Resolution

Agents that encounter an abbreviated crate name they don't recognize (e.g. `tga`, `tm`, `ts`) should call `get_prompt_context()` via the trusty-memory MCP tool to resolve the alias before proceeding. Do not guess — resolve first.

## Crate-Specific Routing

| Crate group | Notes |
|-------------|-------|
| trusty-mpm-* | edition 2024, rust-version 1.88 — engineer must use compatible patterns |
| trusty-memory-mcp | Has embedded Svelte UI in crates/trusty-memory-mcp/ui/ |
| trusty-agents | Consumes trusty-search, trusty-memory-core, trusty-memory-mcp, trusty-symgraph |
| tga (trusty-git-analytics) | Standalone analytics tool, minimal deps on other trusty crates |
