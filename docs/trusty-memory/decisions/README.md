# trusty-memory — Decision Records

Crate-scoped Architecture Decision Records for `trusty-memory`.

## Records

- **[0001 — Frontend/Core Split](./0001-frontend-core-split.md)** — Rationale for splitting the memory palace implementation into a library core (`trusty-common`'s `memory-core` feature) and a thin MCP frontend crate (`trusty-memory`). Amended by [workspace ADR-0066](../../adr/0066-trusty-memory-1x-compatibility-contract.md): the engine moved into `trusty-memory` as `trusty_memory::engine` (#9271).

## Related

- [Workspace ADRs](../../adr/README.md)
- [trusty-memory Overview](../README.md)
