Fixed

- A changelog fragment carrying a second category is now rejected instead of
  silently mis-rendered. Only line 1 is a category and everything after it is
  copied through verbatim, so bare `Changed`/`Added`/`Fixed` lines became body
  text and every bullet landed under the line-1 heading — the 1.3.3
  `4286-retire-trusty-mpm-override-files.md` fragment put all four of its
  categories under `### Removed`, caught only by a human diffing the `--stdout`
  preview. Both the bare-word and the markdown-heading form (`## Changed`) are
  caught, and `scripts/assemble-changelog.sh` names the file and the exact line
  of each. Fenced code blocks are exempt, so a fragment may document the fragment
  format or show example output without tripping the check. The CI gate inherits
  all of it, because `scripts/check_changelog_fragment.sh` asks the assembler
  rather than re-implementing validation. Guarded by
  `scripts/assemble_changelog_selftest.sh`, which replays the real fragment.
