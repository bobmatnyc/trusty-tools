Fixed

- An inline reference with a non-conformant path now fails the lint as
  `ref-traversal`, naming the declaring `file:line` and the offending path,
  instead of being invisible to it (#6605). 38 such references across four
  crates were being declared and checked never; the count of inline references
  the gate resolved went from 37 to 77 once they were made conformant.
- The scan summary reports resolved and rejected references separately —
  `resolved N frontmatter + M inline reference(s), rejected P + Q on path
  shape`. A reference refused on its path resolves against nothing, so it is
  counted apart and cannot hold the scan floor up. `RefScan` and `LintReport`
  gained the matching `rejected` counts.
- `MIN_CODE_REFS` rises from 24 to 51, two-thirds of the corrected measurement,
  so the ratchet still catches the loss it was sized for.
