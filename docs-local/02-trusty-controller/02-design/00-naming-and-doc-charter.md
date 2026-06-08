# DOC-0 — Naming & Documentation Charter

**Status:** Draft — stub
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Decide the tool's real name (binary, crate, directory) and lock the
refinement-doc conventions used across this design set.

## Open Questions / Decisions to Resolve

- Choose among `trusty-controller` vs `trusty-ctl` vs `trusty-installer` vs
  reusing `trusty-tools`. The spec half-rejects `installer` (too install-specific
  for what is also a control plane) and `trusty-tools` (nomenclature clash with
  the monorepo name). `trusty-ctl` reads kubectl-style and is short;
  `trusty-controller` is descriptive as the crate name.
- Disambiguate crate name vs binary name vs directory name. Repo precedent favors
  short binary aliases (e.g. `tga`, `tm`) even when the crate/dir name is longer.
- Decide whether the chosen name should frame the tool as a control plane for the
  *whole* stack (including claude-mpm), not just trusty-tools.

## Dependencies

### Consumes (inputs)
- Spec "Open Questions" section (the naming candidates).
- Repo abbreviation table in `CLAUDE.md`.

### Produces (consumed by)
- The chosen `<name>` is consumed by EVERY downstream doc: filenames, the binary
  declared in the manifest (DOC-2), `<tool>`-shaped contract examples (DOC-1),
  and the `.mcp.json` key / dispatch entries (DOC-5, DOC-8).

## Grounding (exists vs. net-new)

- **Exists:** the repo strongly favors short binary aliases; the `crates/*` glob
  in the root `Cargo.toml` auto-discovers any new member directory.
- **Net-new:** the name itself and its crate/dir/binary mapping; an entry in the
  `CLAUDE.md` abbreviation table.

## Cross-cutting notes

- **Project-identity:** the chosen `<name>` will appear in project-scoped config
  keys; keep it stable since DOC-3's identity convention will reference it.

## TODO

- [ ] Resolve open questions above
- [ ] Define schemas/contracts
- [ ] Review with team
