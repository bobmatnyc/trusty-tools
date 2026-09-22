Added

- `tm-ticketing` (2.1.0) states its taxonomy and behaviour as explicit defaults, names the resolution order a project-root `TICKETING.md` sits at the top of, and carries the canonical skeleton the ticketing agent copies when generating one ([#8376](https://github.com/bobmatnyc/trusty-tools/issues/8376))
  - the component label is now defined by the project's own stack unit — a Cargo crate, an npm/pnpm workspace package, a Python distribution, a Go module — with Cargo as one example and never the definition; the `no-component-label:` prefix is unchanged because `tm issue audit` parses it
  - epic defaults follow the committed tracker + phase-issue pattern at `docs/reference/tracker-phases-pattern.md`: `[EPIC] <outcome>` trackers, `[EPIC_<epic#> PHASE_<n>]` phase issues as native sub-issues, a wholesale-regenerated `phases` block and an amended `deferred` block, and four update triggers. This supersedes the `[EPIC N · Phase M]` naming proposed earlier the same day
  - research output belongs in a committed doc under `research_docs_path` (default `docs/research/<effort>/`) that the tracker links to; no issue body carries findings
  - a per-phase follow-up budget with a severity floor and a due-by window, and a staleness policy whose human decisions are requested per epic as a digest
  - `Refs #N` and `trusty-mpm` as a component label stay fixed at every tier
  - `tm-issues-prune` points at the same policy for the thresholds its Prune phase applies
