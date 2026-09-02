# Documentation Inventory and Code Map Audit — 2026-09-02

## Outcome

The current documentation entry points now agree with the workspace topology
and the implemented CLI/service contracts inspected in this audit. Historical
product specifications remain available, but dated snapshots are explicitly
labelled so they no longer compete with current source, ADRs, behavior
contracts, or crate READMEs.

This is a repository-wide inventory and a targeted currentness audit. It does
not claim that every sentence in every historical research, session, release,
or regression record has been rewritten; those records are evidence tied to a
point in time and should remain stable.

## Snapshot and scope

The final comparison was made against `origin/main` commit `28b6756c9`.

| Surface | Inventory |
|---|---:|
| Cargo workspace packages | 30 |
| Top-level packages under `crates/` | 28 |
| Nested desktop packages | 2 |
| Repository Markdown/MDX files, including this audit and four new crate READMEs | 1,399 |
| Markdown files under `docs/` | 501 |
| Files under `docs/specs/` | 63 (62 behavior/spec documents plus the catalog) |
| Files under `docs/adr/` | 59 (56 ADRs plus process/index material) |
| README files under `crates/`, excluding dependency trees | 65 |

The Markdown inventory includes current guidance and immutable evidence. The
authority and retention rules are documented in
[`docs/reference/documentation-layout.md`](../reference/documentation-layout.md).

## Package-to-code coverage

`cargo metadata --no-deps --format-version 1` is the package authority. The
workspace package table in the root README and the detailed
[`crate map`](../reference/crate-map.md) both cover the full 30-package set with
no missing or extra package names.

All 28 top-level packages now have an owning README. This audit added entry
points for:

- `trusty-kb`
- `trusty-progress`
- `trusty-publish-guard`
- `trusty-sld-lint`

The two nested Tauri packages use the README in their owning `ui/` directory,
which the crate map links directly.

## Currentness repairs

The audit repaired the following drift in current-facing documentation:

- Removed hard-coded 20/21-package claims and made Cargo metadata authoritative.
- Rebuilt the root package index and crate map around all current packages,
  targets, READMEs, and key runtime relationships.
- Replaced the stale mdBook table of contents with progressive navigation to
  current entry points, specs, ADRs, and operational references.
- Updated trusty-analyze from the retired port-7879 HTTP model to its current
  on-demand Unix-socket JSON-RPC and MCP-stdio model.
- Updated trusty-review's analyzer integration to distinguish per-review
  subprocess use from report-mode socket use.
- Replaced the obsolete Phase 0 descriptions of `tcode` and
  `trusty-installer` with their implemented command and lifecycle surfaces.
- Removed release-version pins from installation examples that had become
  false “latest” guidance, and aligned source-build requirements with the
  workspace Rust 1.94 minimum.
- Repaired current-document relative links, including tga contributor docs,
  tcode specs, cutover specs, and DOC-72 references while its source remains in
  an open pull request rather than `main`.
- Removed two retired-daemon-term hits from the public documentation boundary.
- Corrected stale source paths in the trusty-mpm memory/session/search
  architecture note and analyzer configuration in the review README.
- Repaired 19 broken intra-doc rustdoc links in trusty-common and trusty-mpm;
  the zero-baseline rustdoc policy is clean again.

## Historical material

The older `spec/PRD.md`, `spec/ARCHITECTURE.md`, and `spec/COMPONENTS.md` sets
for trusty-agents, trusty-analyze, trusty-common, tga, trusty-memory,
trusty-mpm, and trusty-search describe specific earlier versions or the former
`open-mpm` name. Their status headers now identify them as historical product
baselines. The current navigation points readers to source, crate READMEs,
accepted ADRs, and the workspace behavior-contract catalog first.

## Link and traceability findings

The current/normative local-link scan covered 324 Markdown files. Its remaining
11 unresolved-looking targets are literal filename or spec-reference examples
in the ADR template and SLD/intent-conformance documentation; no current
navigation target is missing.

The required SLD gate resolves all 41 frontmatter and 77 inline references with
zero errors or warnings. That proves declared references are valid, not that
every code unit has a governing specification. The optional gap report found
17,962 of 17,963 discovered code units without backward links and 298 of 326
spec sections without forward links, but its discovery currently includes
untracked dependency material such as `crates/trusty-memory/ui/node_modules`.
Those figures are diagnostic only; the gap report needs dependency-tree
exclusions before it can support a meaningful coverage target.

## Verification

| Check | Result |
|---|---|
| SLD reference gate | Pass: 62 specs, 4,523 code files, 41 frontmatter refs, 77 inline refs, 0 findings |
| Documentation-number registry | Pass: 133 docs, 127 claims, 3 grandfathered, 0 violations |
| ADR consistency | Pass: 56 ADRs consistent |
| Public-document boundary and stale terms | Pass: 27 pages, 5 sections, 0 retired-term hits |
| Generated Markdown ownership | Pass: all generated regions claimed by generated-doc tests |
| Generated docs tests | Pass: trusty-search (3), trusty-memory (2), trusty-analyze (5) |
| Capability manifests | Pass: 7 generated files current |
| Rust source line cap | Pass: 4,363 files, 4 allowlisted, 0 violations |
| Current/normative Markdown links | Pass: no missing real targets |
| Website | Pass: 268 tests, Svelte check with 0 findings, production build |
| Workspace rustdoc links | Pass: 26 crates, 0 broken links against a zero baseline |
| Rust formatting and patch whitespace | Pass (`cargo fmt --all --check`; `git diff --check`) |

## Maintenance rules

1. Derive package membership, names, versions, and targets from Cargo metadata.
2. Treat code as implemented state and draft specs as target state.
3. Give every top-level package one maintained crate README; add extended
   `docs/<product>/` material only when the package needs it.
4. Keep dated research and evidence stable, but label obsolete currentness
   claims and link to the successor.
5. Use generated regions for generated facts and run the owning generated-doc
   test after editing their surrounding README.
6. Keep SLD links semantic and real; do not add references solely to improve a
   coverage number.
