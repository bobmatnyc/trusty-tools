# trusty-agents — Architecture Decision Records

**Crate-specific** ADRs for trusty-agents (formerly open-mpm), in the Nygard
format. These cover decisions local to the trusty-agents crate;
**workspace-wide** decisions live in
[`docs/adr/`](../../adr/). The ADR process, numbering, and status lifecycle are
documented in the [workspace ADR README](../../adr/README.md). This directory
maintains its own independent numbering sequence.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](./0001-ndjson-subprocess-ipc.md) | NDJSON-over-stdin/stdout subprocess IPC for sub-agents | Superseded by workspace ADR-0024 |
| [0002](./0002-model-agnostic-credential-routing.md) | Model-agnostic credential routing | Accepted |
| [0003](./0003-daemon-process-model.md) | Daemon process model — one daemon per agent identity, one PM per project, bounded coding-agent fan-out | Accepted |

---

[← Back to trusty-agents docs index](../README.md)
