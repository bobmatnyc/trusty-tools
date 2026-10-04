# 0066. trusty-memory runs the sync connector and the dream cycle as daemon-supervised child processes

- **Status:** Proposed
- **Date:** 2026-10-04
- **Scope:** crate `trusty-memory` (two hidden subcommands, one child supervisor, sync and dream RPCs); crate `trusty-common` (reused supervisor, RPC client and dream passes; changes routed through the Architect)
- **Reversibility Cost:** Low while nothing is built; Medium after Stage 2 of [DOC-80](../specs/DOC-80-memory-sync-design.md), when status keys and RPC names have readers
- **Decision Drivers:** owner ruling 2026-10-04 22:29Z (connector in trusty-memory, separate background process under the daemon, "do not silently reduce it to an in-process async task"); owner decision 2026-10-04 on DOC-80 OQ-1 (option (a), child process); owner scope addition 2026-10-04 (the dream cycle also moves out of the daemon process into a daemon-supervised child); store file lock; recall must never block on sync or dreaming
- **Supersedes / Superseded by:** —

## Context

Bob ruled that the sync connector runs as a separate background process under the daemon, "operationally analogous to the dream cycle". He then decided (2026-10-04) that the dream cycle itself also leaves the daemon process. Verified facts (DOC-80 §2):

- Today the dream cycle is an in-process tokio task per palace (`crates/trusty-memory/src/dream_scheduler.rs:122`, `crates/trusty-common/src/memory_core/dream/dreamer.rs:145`). Its passes mutate the in-memory drawer table and call `forget_for_maintenance` directly on a `PalaceHandle` (`crates/trusty-common/src/memory_core/dream/cycle.rs:385-389`).
- A second process cannot open a palace store for writing while the daemon holds it (`crates/trusty-common/src/memory_core/store/concurrent_open.rs:3`, `:63-67`). Any child must reach the store through the daemon.
- Maintenance runs only in the process holding the per-data-root lease (`crates/trusty-common/src/memory_core/maintenance_lease.rs:1-17`, `crates/trusty-common/src/memory_core/registry.rs:320`).
- `trusty-common` has a child supervisor with exponential backoff and a give-up flag (`crates/trusty-common/src/embedder_client/supervisor.rs:98`; defaults `crates/trusty-common/src/embedder_client/supervisor_config.rs:84-85`) and a parent-death helper (`crates/trusty-common/src/parent_death.rs`).
- Dream merges are not durable today: `merge_into` changes only the in-memory table (`crates/trusty-common/src/memory_core/dream/helpers.rs:125-148`) and `flush` saves only the L1 cache (`crates/trusty-common/src/memory_core/retrieval/handle.rs:571-581`). This is open defect [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172).

Counter-precedent: BM25 once ran as a supervised subprocess and was collapsed in-process ([#5329](https://github.com/bobmatnyc/trusty-tools/issues/5329), `crates/trusty-memory/src/bm25_lane.rs:3-4`). Both children here differ: the connector does network I/O against slow or hostile endpoints and holds credentials; the dream cycle runs long CPU passes and optional LLM calls, and both were explicitly ruled separate.

Options considered: (a) child processes supervised by the daemon; (b) in-process tasks; (c) independent launchd services.

## Decision

We will take option (a) for both children, under one supervision model:

1. Each child is a hidden subcommand of the existing `trusty-memory` binary (proposed `sync-connector` and `dream-worker`). No new binary is installed.
2. Only the daemon holding the maintenance lease spawns them: the connector when at least one project has sync bound, the dream worker unless `TRUSTY_DREAM_DISABLED` is set. One data root has at most one of each.
3. One supervisor instance per child: restart after any exit the daemon did not request, exponential backoff, give-up after a bounded number of restarts in a window, give-up shown in status. Each child arms parent-death and exits when the daemon dies. Shutdown is a daemon-sent stop with a grace period.
4. Each child reaches the store only through daemon RPCs on the existing socket, authenticated by a secret the daemon hands the child at spawn. Neither child opens palace files.
5. Every store mutation a child asks for is applied by the daemon as one atomic unit, with its journal record, or not at all. A child crash therefore never leaves a partial change.
6. Recall, remember and forget never wait on either child. Neither child serves recall. When a child is down, manual triggers (`memory.dream_run`, `dream_consolidate_room`) return an "unavailable" error; the daemon never falls back to an in-process pass.

Mechanics: DOC-80 §3.1 (shared supervision) and §10 (dream worker).

## Consequences

- A hung endpoint, a slow LLM call, a long dedup pass or a child crash stays in the child; the daemon and recall keep running (DOC-79 R-PROC-3, R-DREAM-5).
- New daemon RPCs become a versioned internal contract: sync outbox/ingest/report and dream next/snapshot/neighbours/apply/maintain/report.
- The dream passes in `trusty-common` must be split into a decide step (child) and an apply step (daemon). That is runtime work in code the runtime owner is changing for [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) and [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173); it is sequenced after those fixes and assigned by the Architect.
- Supervision code that trusty-memory does not run today must be wired in and tested (rung 5, process lifecycle).
- Option (c) stays available if either child ever needs to run without the daemon.

## Related Decisions

Vetted against prior ADRs on 2026-10-04:

- **ADR-0032 (UDS is the inter-service transport; console is the only HTTP surface):** Consistent — both children talk to the daemon over UDS and serve no HTTP; the connector is only an outbound HTTPS client.
- **ADR-0043 (cargo bin policy):** Consistent — no new binary.
- **ADR-0065 (trusty-events as its own supervised daemon):** Consistent — same reasoning that a first-class concern gets process isolation; here the supervisor is the trusty-memory daemon, not launchd.
- **ADR-0028 (recall tiers):** Consistent — recall tiers are unchanged; Tier C records do not sync in v1.
- **ADR-0051 (palace `owner`/`project` fields):** Consistent — sync identity stays out of `palace.json` (DOC-80 OQ-3, decided 2026-10-04); ADR-0051 is amended afterwards, not by this ADR.
