# Documentation Layout Reference

Documentation is organized by audience and authority. Package name alone does
not determine a fixed directory template; lightweight libraries need less
surface area than products with user guides, behavior specs, research, and
release evidence.

## Authority order

When two documents disagree, use this order and repair the lower-authority
surface:

1. Current source and Cargo manifests for implemented behavior and package
   topology.
2. Accepted ADRs for architectural decisions.
3. Behavior-contract specs for intended behavior, with status and revision
   interpreted explicitly.
4. Current crate READMEs and workspace reference/guides for user-facing usage.
5. Dated research, plans, sessions, audits, regression snapshots, and
   changelogs as historical evidence.

Draft specs describe target state, not proof that code has landed. A current
README must not present draft behavior as implemented.

## Current entry points

- `README.md`: workspace orientation; links to the live package map.
- `crates/<directory>/README.md`: required package entry point for top-level
  workspace members; installation/usage or library quick start belongs here.
- Rustdoc in `crates/<directory>/src/`: public API contract.
- `docs/<product>/README.md`: optional extended index for products that need
  more than a crate README.
- `docs/getting-started/`, `docs/reference/`, `docs/architecture/`, and
  `docs/distribution/`: cross-package current guidance.

Use [crate-map.md](crate-map.md) for the package-to-code-to-doc mapping.

## Normative engineering records

### Behavior-contract specs

Workspace specs live under [`docs/specs/`](../specs/README.md). New specs follow
[DOC-38 — Spec-Linked Documentation](../specs/spec-linked-documentation.md),
including stable section IDs and explicit status. `scripts/check_sld.sh`
validates declared references; the `sld-lint gap-report` command reports
unmapped code units and spec sections but is not itself a completeness gate.

### Architecture decisions

Workspace ADRs live under [`docs/adr/`](../adr/README.md). They record why a
hard-to-reverse choice was made. [DOC-46](../specs/DOC-46-adr-standard.md)
defines the format and `scripts/check_adr.sh` enforces corpus consistency.
Package-local decision records may live under `docs/<product>/decisions/`.

## Historical evidence

The following are point-in-time records and are not silently rewritten to look
current:

- `research/`: investigations and evaluated alternatives.
- `plans/`: intended implementation sequences.
- `sessions/`: engineering-session narratives.
- `regression-testing/`: measurements tied to a version/date/corpus.
- `audits/` and `reporting/`: dated findings and generated evidence.
- `CHANGELOG.md` and `changelog.d/`: release history.
- `_archive/` or `archive/`: explicitly retired material.

If a historical document is still reachable from current navigation, label it
as historical and link to the current replacement.

## Publication boundary

The public website publishes only the allowlisted pages in
[`docs/public-manifest.tsv`](../public-manifest.tsv). The manifest is a security
and audience boundary, not an inventory of the internal documentation tree.
`scripts/check_public_docs.sh --stale` validates its paths and retired-term
ratchet.

## Keeping documentation mapped to code

- Derive package membership and targets from `cargo metadata`; do not maintain
  standalone numeric crate counts.
- Link CLI claims to the owning clap command or generated tool table.
- Put generated facts only inside `<!-- BEGIN GENERATED: ... -->` regions and
  validate them with the owning `generated_docs` test.
- Use SLD references only where a real behavior spec governs the code; never
  add decorative spec links to make coverage numbers look better.
- Update current docs with the code change. Preserve dated evidence, adding a
  status note or successor link when its old context could mislead a reader.
