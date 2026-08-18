Fixed

- Repositories are cloned with their whole history. `taudit clone` appended
  `--depth=1`, so tga read exactly one commit per repository and every
  deliverable said `commits=1`, `authors=1`, a period whose start equalled its
  end, and one author credited with the entire tree. Measured on
  `BurntSushi/xsv`: 1 commit by 1 author on 2025-04-24, against a real history
  of 407 commits by 30 authors from 2014-09-01. Every CSV was a header and one
  row.
