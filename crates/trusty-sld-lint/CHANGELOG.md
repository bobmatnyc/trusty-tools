# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- Initial release: `sld-lint`, the Spec-Linked Documentation (DOC-38) linter — substantially delivers DOC-38 §10 follow-up **F1** ([#2854](https://github.com/bobmatnyc/trusty-tools/issues/2854)).
  - **Reference resolution (always, everywhere in scope):** every declared reference — inline `# Spec References` blocks across `crates/**` and `spec_refs:` frontmatter in `docs/specs/**/*.md` — must resolve (repo-root-relative path exists AND a matching `{#SPEC-…}` anchor exists, revision-tolerant); the anchor must equal its id (§2.1 self-check); paths may not traverse via `..`; frontmatter must be schema-valid (§2.5).
  - **Spec-document conventions (opted-in specs by default, ALL specs under `--strict`):** the bold-field header block (§4.2), the catalog-row requirement (§4.5), `{#SPEC-…}` anchor grammar and anchor↔`**ID:**` agreement (§4.3).
  - **Grandfathering:** default mode applies full spec-document checks only to files that carry `spec_refs:` frontmatter (existing specs predate the retrofit, DOC-38 §10 F5/F6), while reference resolution runs everywhere. Documented pre-existing exceptions (DOC-28 collision, DOC-34/DOC-37 catalog gaps) are grandfathered in `.sld-lint-allowlist.tsv`, a ratchet that can only shrink. `--strict` is the eventual post-retrofit mode.
  - **Grammar reuse:** built entirely on `trusty_common::sld` (the lightweight `sld` feature) — one grammar, never a second parser.
  - **Wiring:** `scripts/check_sld.sh` wrapper, `.github/workflows/sld-lint.yml` CI job, and a `sld-lint` pre-commit hook, all mirroring the 500-SLOC line-cap gate's ergonomics.
