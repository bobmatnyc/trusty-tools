# DOC-2 — Stack Manifest/BOM + Version & Changelog Advertisement (FOUNDATIONAL)

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Define the manifest/BOM/lockfile that pins known-good tool-version combinations
and a "stack version", doubling as the controller's tool registry (binary
name/path, pinned version, expected `contract_version`).

## Open Questions / Decisions to Resolve

- Pick format & location: TOML vs JSON; shipped-in-binary vs fetched; a
  system-level manifest vs per-project override.
- Define each entry's contents: member id, binary name, install source
  (`cargo install <crate>`), pinned version, expected `contract_version`,
  daemon-vs-CLI-only flag, UI URL.
- Define "stack version" plus a lockfile of known-good version tuples.
- Choose a structured, parseable changelog format per tool to support
  "changelog headlines" (a spec requirement).
- Fix the discovery rule: the controller enumerates members **from the manifest**
  and never probes or hard-codes them.

## Dependencies

### Consumes (inputs)
- DOC-0 (the chosen `<name>`).
- DOC-1 (the `contract_version` advertised per member).

### Produces (consumed by)
- DOC-5, DOC-6, DOC-7, DOC-8, DOC-9.

## Grounding (exists vs. net-new)

- **FULLY NET-NEW.** There is no manifest/BOM, no `stack_version`, and no
  structured changelog today.
- `cargo install` is per-crate; each crate has an unstructured `CHANGELOG.md`.
- Per-crate independent versioning is a hard constraint (the workspace has no
  shared version field).

## Cross-cutting notes

- **Security / secrets:** no secrets in the manifest (it may be committed/shipped;
  install sources and URLs only, never credentials).

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
