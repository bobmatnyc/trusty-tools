# trusty-mpm — research

Investigation docs, audits, decision documents, and reconstructed design docs for the
`trusty-mpm` crate.

## Naming convention

`{topic}-{YYYY-MM-DD}.md` or `{topic}-decision-{YYYY-MM-DD}.md`.

## Contents

- **[prd-2026-05-29.md](./prd-2026-05-29.md)** — Reconstructed Product Requirements
  Document. Recovers the original product intent (single-daemon-per-machine coordinating
  multiple Claude Code processes; Rust successor to `claude-mpm`) and annotates each
  functional/non-functional requirement with current implementation status.
- **[architecture-spec-2026-05-29.md](./architecture-spec-2026-05-29.md)** — Reconstructed
  technical specification. Covers the process/coordination model, module breakdown, data
  model, instruction assembly, agent/skill deployment, HTTP API + MCP surfaces, filesystem
  layout, configuration, distribution, and a prioritized gaps-and-deviations list. Marks
  INTENDED design vs CURRENT IMPLEMENTATION wherever they differ.
- **[tm-services-discovery-spec-2026-05-28.md](./tm-services-discovery-spec-2026-05-28.md)** —
  Engineering spec for the `tm services` canonical service-discovery CLI (manifest schema,
  discovery engine, CLI subcommands, phased implementation plan).

## Related

- [Parent index](../README.md)
- [crate README](../../../crates/trusty-mpm/README.md)
