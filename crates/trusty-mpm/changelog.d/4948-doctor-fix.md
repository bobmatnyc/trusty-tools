Added

- `tm doctor --fix` repairs the findings tm can prove it owns, instead of only reporting them (closes [#4948](https://github.com/bobmatnyc/trusty-tools/issues/4948))
  - dry run by default: it prints every change per item, with paths, and writes nothing until `--yes`
  - covers `skill_staleness` (redeploy from the bundled asset), `hooks_contamination` (strip tm's own hook entries from this project's `.claude/settings*.json`, backing the file up first), and `push_guard` (retrofit the cross-branch `pre-push` guard)
  - `legacy_sources` findings under `~/.claude` are reported as REFUSED and never deleted — a copy there may be hand-edited, and a directory name cannot prove otherwise
  - a hand-edited (FROZEN) skill and a foreign `pre-push` hook are refused in both modes; `--include-frozen` now works with either `--fix` or `--fix-skills`
  - `--fix-skills` gained a preview through `--fix`, and its output now names the exact file it repaired at each tier
