Added

- `trusty-audit clone <owner/name>...` acquires repositories the recipient has
  never checked out, into the working directory's `repos/` area — via
  `gh repo clone`, reusing the credential `gh auth login` already resolved. A
  clone is built under `state/clone-staging/` and renamed into place only after
  `gh` exits zero AND the tree verifies as a real checkout, so neither an
  interrupted run nor a commitless repository (which clones with exit 0 and no
  commits) is ever promoted as whole. One repository failing is a named gap and
  the run continues, exiting 2 so a shell chain does not proceed against an
  incomplete set; only every repository failing aborts. Clones are shallow by
  default and disk use is reported; `--budget-gb` stops new clones from
  STARTING once that much is on disk and does not cap a clone already running
  (#5215, DOC-68 §8 / §14 Q2).
