Fixed

- The PM instruction floor's "Customizing PM Behavior" table advertised only the legacy `.trusty-mpm/*.md` override files and never mentioned the `CLAUDE.md` named-section channel that outranks them (`HOST_FILES` in `core/claude_md_sections.rs` scans `CLAUDE.md` before `.trusty-mpm/INSTRUCTIONS.md`), so the floor routed every PM session to the lower-precedence surface. `sections/non-overridable-rules.md` now documents named-section markers as the one customization channel, lists all eight recognized tokens and the three `fixed`-tier ones that can never be overridden, and states plainly that the `.trusty-mpm/` override files are still read by the current binary but [#4286](https://github.com/bobmatnyc/trusty-tools/issues/4286) removes them.
- Fixed a broken pointer in `sections/non-overridable-rules.md` to the deleted `PM_INSTRUCTIONS.md` (removed by [#4183](https://github.com/bobmatnyc/trusty-tools/issues/4183)) — repointed at the CORE section's Prohibitions table — and fixed mojibake (`SS` for `§`).
- `core.md`'s Model Selection Protocol undercounted the model set ("sonnet or haiku", omitting opus — the mandatory choice for coding tasks per its own routing table two lines below), and its PM Allowlist listed `push` as an allowed git op, contradicting P7 ("branch/push/rebase/merge/tag -> Version Control") one section up. Both corrected to match the rest of the floor.

Changed

- Deduplicated the commit/PR attribution footer text, which appeared three times in the resolved PM prompt (`core.md`, `workflow.md`, `framework-guaranteed-conventions.md`). The two non-canonical copies now point to the single canonical statement in `framework-guaranteed-conventions.md`; the footer text itself is unchanged.
