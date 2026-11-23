# Restate Evaluation for open-mpm

**Date**: 2026-04-25
**Researcher**: Research Agent (claude-sonnet-4-6)
**Purpose**: Assess whether Restate should replace or underpin open-mpm's ad-hoc process management with durable execution capabilities

---

## Executive Summary

Restate is a compelling durable execution engine built in Rust, but it is architecturally incompatible with open-mpm's core design goal of being a single self-contained binary. Restate requires a separate server process that your application registers handlers with over HTTP — it is a middleware layer, not an embeddable library. For open-mpm in its current local-first, single-binary form, Restate would add significant operational complexity and a non-trivial licensing burden. The recommendation is **No-Go for Restate adoption now**, with a clear path to revisit if open-mpm ever moves to a distributed, multi-node deployment model.

---

## 1. What is Restate?

Restate is a durable execution engine written in Rust that solves the problem of making distributed workflows reliable across failures. Its core promise: write straightforward sequential code; Restate ensures it runs to completion even if the process crashes mid-execution.

### Problem It Solves

Without durable execution, a multi-step workflow that crashes at step 7 of 10 must restart from step 1. Restate journals every completed step so that on restart, completed steps return their cached results and execution resumes from the failure point. This eliminates defensive retry logic, idempotency boilerplate, and the risk of re-running side effects (LLM calls, database writes, API calls).

### Programming Model

The programming model is handler-based. You define services as structs with handler methods. Handlers receive a `Context` object. Every side-effecting operation is wrapped in `ctx.run(|| ...)` calls, which Restate records in a durable journal. If the process crashes and restarts, Restate replays the journal: completed `ctx.run` steps return their stored result without re-executing; incomplete steps execute normally.

```rust
// Conceptual sketch — not actual open-mpm code
#[restate_sdk::service]
impl MyService {
    async fn my_handler(ctx: Context, task: String) -> Result<String> {
        let result = ctx.run(|| call_llm(&task)).await?;  // journaled
        let next   = ctx.run(|| call_tool(&result)).await?;  // journaled
        Ok(next)
    }
}
```

Durable Promises allow handlers to be paused until an external event arrives (e.g., human-in-the-loop approval). Workflows (a special handler type) execute exactly once per workflow instance and support structured step composition.

### Language Support

TypeScript, Java, Kotlin, Python, Go, and Rust are all supported. The Rust SDK (`restate-sdk` on crates.io, `restatedev/sdk-rust` on GitHub) is actively maintained, reached v0.9.0 in February 2026, and is MIT-licensed independently of the server.

---

## 2. Technical Fit Analysis

### Deployment Model: External Server Required

This is the critical finding. Restate is **not an embeddable library**. It operates as a separate server process (`restate-server` binary) that sits between clients and your application's business logic.

The flow is:
```
Client Request
    --> Restate Server (separate process, owns journal + state)
        --> HTTP callback to your service handlers
            --> your handler code executes
        <-- response returned to Restate
    <-- Restate returns to client
```

Your service handlers are HTTP endpoints. The Restate server calls them via HTTP, journals the results, and manages retries. Your application does not call Restate — Restate calls your application.

This is a fundamentally different architecture from open-mpm's model, where the PM process is the orchestrator and sub-agents are spawned subprocess children. To adopt Restate, open-mpm would need to:

1. Run a separate `restate-server` binary alongside the main binary
2. Expose each "agent" as an HTTP service endpoint
3. Replace subprocess spawn/IPC with Restate service invocations
4. Abandon the single-binary goal entirely

The Restate server is a single binary (ships as one statically-linked Rust binary with embedded RocksDB), so its operational footprint is modest — but it is unambiguously a separate process with its own lifecycle.

### Subprocess Spawning Pattern

open-mpm's current pattern — PM spawns sub-agent subprocesses, communicates via NDJSON over stdin/stdout — has no direct equivalent in Restate. Restate's model is HTTP handlers, not subprocesses. You could implement an adapter where a Restate handler spawns a subprocess, but this defeats most of the value: Restate journals HTTP round-trips, not arbitrary subprocess calls. The subprocess's internal state (LLM streaming, partial results) would remain ephemeral.

### Long-Running LLM Calls

Restate handles long-running operations via durable suspension: a handler can yield (await a Durable Promise) while waiting for an external event, and the Restate server will resume it when the event arrives. This is well-suited to human-in-the-loop workflows but has a specific limitation for streaming LLM responses: Restate journals completed step results, not streaming tokens. A streaming LLM call would need to complete fully before being journaled. Token-by-token streaming to a user interface is not a Restate use case — Restate is about durability of completed steps, not live streaming.

### Interrupt / Pause / Resume

Restate's Durable Promises and workflow suspension do support pause/resume semantics, but only at the `ctx.run()` step boundary. You cannot pause a handler in the middle of an async computation that is not wrapped in a `ctx.run`. For open-mpm's use case — interrupting a running LLM generation mid-stream — Restate does not provide this. The granularity is coarser: between steps, not within a step.

### Overhead and Resource Cost

The Restate server uses embedded RocksDB for its journal and state store. This adds:
- A second process (additional ~50-100MB memory depending on workload)
- RocksDB I/O on every journaled step (typically single-digit milliseconds per step)
- HTTP round-trip overhead for each handler invocation (localhost, so sub-millisecond for the transport itself)

For a local development tool where LLM calls dominate latency (seconds to minutes), the Restate overhead is negligible in absolute terms. The operational burden (running two processes, managing Restate's data directory) is the more meaningful cost.

### Tokio Compatibility

Restate's Rust SDK is fully tokio-based. The SDK starts its own HTTP listener using tokio's runtime. This means you would have two tokio runtimes in the same process if you tried to run your application logic and the Restate SDK HTTP server in the same binary — which is technically possible but architecturally confusing. More naturally, your application logic becomes a separate process from the Restate server.

### Single-Binary Goal

Restate is incompatible with this goal. The architecture requires at minimum two processes: the Restate server and your service handlers. There is no in-process embedding mode.

---

## 3. Licensing

### Restate Server

**License**: Business Source License 1.1 (BUSL-1.1)

BUSL is not an open source license (not OSI-approved). The key terms:

- **Non-production use**: Freely permitted (development, testing, internal evaluation)
- **Production use**: Permitted with the Additional Use Grant, which allows you to run Restate for your own workflows and internal services
- **Restricted use**: You cannot offer "a managed service that lets third parties register their own Restate service endpoints" through your platform. This targets cloud providers building Restate-as-a-service, not typical application developers
- **Conversion**: Four years after each release, the license converts to Apache 2.0

**For open-mpm's use case**: Running Restate to orchestrate open-mpm's own agent workflows appears to fall within the Additional Use Grant's permitted scope. However, if open-mpm were ever offered as a hosted multi-tenant service where end users register their own agent endpoints, that would approach the restricted territory. The language has acknowledged ambiguity (noted in community discussion).

### Restate Rust SDK

**License**: MIT

The SDK is MIT-licensed and freely usable without restriction. The licensing concern is exclusively on the server binary.

### CLA Requirements

No CLA information was found for either the server or SDK repositories. Standard GitHub contribution flow appears to apply.

---

## 4. Alternatives for Durable Execution in Rust+Tokio

### Option A: Temporal (Rust SDK)

Temporal is the most mature durable execution system. However:
- The native Rust SDK is pre-release (not production-ready as of April 2026)
- Temporal's Rust core powers other language SDKs internally, but the end-user Rust SDK API is unstable
- Temporal requires an external server (heavier than Restate: requires a database like PostgreSQL or Cassandra)
- License: MIT for the server; business-friendly

**Verdict for open-mpm**: Not suitable now. More operationally complex than Restate. Rust SDK immaturity is a blocker.

### Option B: iopsystems/durable

A durable execution engine for Rust, MIT/Apache-2.0 dual-licensed. Key differentiator: supports running the worker **embedded within an existing application** (single-process mode). Uses WebAssembly components as the workflow unit, which adds an unusual compilation step (`cargo-component`).

**Verdict for open-mpm**: Interesting for single-binary compatibility, but WASM component compilation is a significant friction point that clashes with open-mpm's straightforward Rust binary architecture. Immature ecosystem.

### Option C: Custom Journaling with redb

Build a lightweight durable execution layer directly on top of redb (already in open-mpm's dependency set) with tokio task supervision.

Design sketch:
- On workflow step start: write `{step_id, status: "started", input}` to redb
- On step completion: write `{step_id, status: "completed", output}` to redb
- On startup: scan for `"started"` steps and resume them
- Use tokio's `JoinSet` or `AbortHandle` for task supervision

Pros: No external dependency, no new process, fully embeddable, redb is already present, Apache/MIT licensed.

Cons: You build the replay logic yourself. Correctness requires careful attention to exactly-once semantics. No ecosystem of tooling (UI, observability).

**Verdict for open-mpm**: The most natural fit for current goals. The scope of durable state open-mpm needs is modest: journal which agent phases completed, store their outputs. This is achievable in ~300-500 lines of Rust without a framework.

### Option D: tokio Task Supervision (No Journaling)

Use `tokio::task::JoinHandle`, `AbortHandle`, and `select!` for structured concurrency. Implement a task registry that tracks running agent invocations in memory (not persisted). On crash, state is lost — but add restart logic that re-reads workflow definition and re-runs from the last known checkpoint saved to a simple JSON file.

Pros: Zero new dependencies. Minimal complexity. Already close to what open-mpm does.

Cons: Not truly durable — crash recovery requires a restart mechanism and manual checkpoint discipline.

**Verdict for open-mpm**: Appropriate for the current phase. Implement crash-safe checkpoints as a JSON file written after each phase completes. This is 90% of the benefit with 10% of the complexity.

---

## 5. Verdict: Go / No-Go

**Recommendation: No-Go for Restate adoption in open-mpm at this time.**

### Rationale

| Criterion | Assessment |
|---|---|
| Single-binary goal | Incompatible — Restate requires a separate server process |
| Local-first, no dependencies | Incompatible — Restate embeds RocksDB and requires its own runtime |
| Subprocess IPC pattern | Incompatible — Restate uses HTTP handlers, not subprocess stdio |
| Tokio integration | Compatible at the SDK level, but requires separate process architecture |
| LLM streaming support | Weak — Restate journals completed steps, not streaming tokens |
| License clarity | Acceptable for internal use; BUSL ambiguity increases if multi-tenant hosting is planned |
| Rust SDK maturity | Active development, v0.9.0, MIT-licensed — SDK itself is usable |
| Migration cost from current ad-hoc model | Very high — requires restructuring the entire execution model |

### What We Would Gain from Restate

- Automatic retry with backoff on agent step failure
- Crash-safe workflow resumption (no re-running completed steps)
- Durable Promises for human-in-the-loop pauses
- An established operational model for running agent workflows
- Observability tooling (Restate UI shows workflow state)

### What We Would Lose or Accept

- Single-binary deployment model (must run two processes)
- Direct subprocess IPC (must replace with HTTP handler endpoints)
- Streaming LLM output (incompatible with step-level journaling model)
- Simplicity of current architecture
- BUSL licensing dependency for the server

### Recommended Path Forward for Durability

For open-mpm's current phase, implement lightweight crash-safe checkpointing using redb (already a dependency):

1. Write a `WorkflowJournal` backed by a redb table
2. On each phase start: insert `{workflow_id, phase_id, status: "pending", input}`
3. On each phase complete: update to `{status: "completed", output}`
4. On startup: detect incomplete workflows and offer to resume or restart
5. Use tokio `JoinSet` for concurrent phase execution and cancellation

This delivers the core durability property (no re-running completed phases after crash) with zero new dependencies, zero new processes, and no architectural restructuring.

### When to Revisit Restate

Restate becomes worth serious reconsideration if open-mpm evolves to:
- Multi-node deployment (PM orchestrators running across multiple machines)
- Multi-tenant hosting where workflow durability SLAs are user-facing commitments
- Human-in-the-loop workflows requiring reliable multi-day pause/resume
- Workflow observability as a product feature (Restate's UI provides this)

At that inflection point, the operational cost of running a Restate server is justified, and the architectural fit improves significantly.

---

## Sources

- [Restate homepage](https://restate.dev)
- [Restate Rust SDK — GitHub](https://github.com/restatedev/sdk-rust)
- [Restate Rust SDK — docs.rs](https://docs.rs/restate-sdk/latest/restate_sdk/)
- [Restate durable execution concepts](https://docs.restate.dev/concepts/durable_execution)
- [Restate architecture reference](https://docs.restate.dev/references/architecture)
- [Restate LICENSE (BUSL-1.1)](https://github.com/restatedev/restate/blob/main/LICENSE)
- [Building a modern Durable Execution Engine from First Principles — Restate blog](https://www.restate.dev/blog/building-a-modern-durable-execution-engine-from-first-principles)
- [Restate server — GitHub](https://github.com/restatedev/restate)
- [Temporal Rust SDK — crates.io](https://crates.io/crates/temporalio-sdk)
- [Temporal: Why Rust powers Core SDK](https://temporal.io/blog/why-rust-powers-core-sdk)
- [iopsystems/durable — GitHub](https://github.com/iopsystems/durable)
- [Durable execution topics — GitHub](https://github.com/topics/durable-execution?l=rust)
- [Restate BUSL license discussion — Hacker News](https://news.ycombinator.com/item?id=42821705)
- [Durable Execution Patterns for AI Agents — Zylos Research](https://zylos.ai/research/2026-02-17-durable-execution-ai-agents)
- [Rise of Durable Execution Engines — Kai Waehner blog](https://www.kai-waehner.de/blog/2025/06/05/the-rise-of-the-durable-execution-engine-temporal-restate-in-an-event-driven-architecture-apache-kafka/)
