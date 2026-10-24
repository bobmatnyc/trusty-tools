# DOC-46 — Architecture Decision Records (ADR) as First-Class Documentation Artifact

**Status:** ACCEPTED
**Subsystem:** Documentation / Architecture governance
**Owner:** Architecture / Technical Leadership
**Last-updated:** 2026-10-24
**Spec ID:** `SPEC-ADR-01` (DOC-46)
**Builds on:** the `tm-adr` skill and its standard,
[`content/skills/tm-adr/references/adr-standard.md`](../../content/skills/tm-adr/references/adr-standard.md);
DOC-38 (Spec-Linked Documentation / SLD); `docs/adr/README.md`

---

## 1. Scope

The generic ADR standard is
[`content/skills/tm-adr/references/adr-standard.md`](../../content/skills/tm-adr/references/adr-standard.md),
shipped with the `tm-adr` skill. It defines when to write an ADR, the file
name, the metadata fields, the core sections, the status lifecycle, the
claim-then-populate numbering, the consistency-vetting protocol and its
verdict codes, the index, and the governance rules. That file is the single
source; this spec does not restate it.

This spec holds only the rules that apply to trusty-tools and to no other
project: the ADR, Spec and Req cross-linking rule (§2), the per-crate
decision directories (§5), the CI gate (§6), and the exemption for
ADR-0001..0013 (§7). Sections §3 and §4 stay as pointers because accepted
ADRs cite them by number (#10081).

## 2. ADR, Spec and Req

ADRs are a documentation artifact class equal to Specs (DOC-38) and
Requirements (DOC-43). An ADR records *why* a choice was made, a Spec records
*how* a subsystem works, and a Req records *what* must be true.

Cross-linking rule:

- Specs cite the ADRs that shaped them.
- ADRs cite the Specs that implement them.
- Reqs may cite ADRs when a requirement encodes a decision constraint.

## 3. ADR Template & Structure

The metadata block, the core sections and the `## Related Decisions`
consistency-vetting protocol, with its verdict codes, are defined in
[`content/skills/tm-adr/references/adr-standard.md`](../../content/skills/tm-adr/references/adr-standard.md)
under "Metadata Fields", "Core Sections", "Consistency Vetting" and "Related
Decisions". The template for this repository is
[`docs/adr/template.md`](../adr/template.md).

## 4. ADR Status Lifecycle

The five status values, the successor links and the immutability rule (an
accepted ADR changes only by a new ADR that supersedes or amends it, never by
an edit to its Context, Decision or Consequences) are defined in
[`content/skills/tm-adr/references/adr-standard.md`](../../content/skills/tm-adr/references/adr-standard.md)
under "Status Lifecycle" and "Governance".

## 5. Per-Crate Decision Directories

Workspace-wide decisions live in `docs/adr/`. A crate may keep its own
decisions in `docs/<crate>/decisions/`, numbered independently of
`docs/adr/` and of every other crate. Workspace ADRs and crate decisions may
cite each other.

Each `docs/<crate>/decisions/` directory has a `README.md` that names every
`NNNN-*.md` record in that directory. The CI gate checks that listing and
nothing else in those directories (§6).

## 6. Mechanical Enforcement & CI Gate

`scripts/check_adr.sh` is a byte-identical copy of the skill's
`content/skills/tm-adr/scripts/check_adr.sh`. The `Doc numbers` workflow
(`.github/workflows/doc-numbers.yml`) runs these steps in order:

```bash
cmp scripts/check_adr.sh content/skills/tm-adr/scripts/check_adr.sh
bash scripts/check_adr.sh --self-test
bash scripts/check_adr.sh --verify-self
bash scripts/check_adr.sh --require-metadata-from 0014 --extra-dirs 'docs/*/decisions'
```

- The `cmp` step fails when the repository copy drifts from the content
  copy. Change the content copy, then copy it to `scripts/check_adr.sh`.
- `--self-test` runs the script's own fixtures; `--verify-self` checks its
  `body-sha256` header against its body.
- The gate enforces the checks listed under "Mechanical Checks" in the skill
  standard on `docs/adr/`. `--require-metadata-from 0014` applies the
  metadata and vetting checks from ADR-0014 on (§7). `--extra-dirs` applies
  the README-listing check to each `docs/*/decisions` directory (§5).
- The script needs bash 4 or later. On macOS, run it with a Homebrew `bash`
  on `PATH`, not `/bin/bash`.

`scripts/check_doc_numbers.sh` is a separate lint in the same workflow. It
refuses two files that claim the same DOC-N or ADR number.

Semantic consistency between two Accepted ADRs is not checked mechanically.
The reviewer reads the swept ADRs and confirms the `## Related Decisions`
verdicts.

## 7. Grandfathered ADR-0001..0013

ADR-0001..0013 predate the metadata block and the `## Related Decisions`
section. They are exempt from the metadata and vetting checks, which is why
the gate passes `--require-metadata-from 0014`. Their Context, Decision and
Consequences are not rewritten to match the template. Their status, successor
links, numbering and index rows are still checked.

## Appendix: References

- [`content/skills/tm-adr/references/adr-standard.md`](../../content/skills/tm-adr/references/adr-standard.md)
  — the generic ADR standard
- [`docs/adr/README.md`](../adr/README.md) — authoring steps for this
  repository
- DOC-38 — Spec-Linked Documentation (SLD) standard
- Michael Nygard, "Documenting Architecture Decisions" (2011):
  https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions
