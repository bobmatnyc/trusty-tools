Added

- The grounding pass now checks every finding's cited path, line and traced
  symbol against the checkout while that checkout is still on disk, and records
  `confirmed` / `stale` / `unreachable` per citation on the manifest's
  repository entry as `citation_verdicts`. Trace verdicts were produced only at
  synthesis, which for a delivered bundle runs against manifests that ship with
  no git checkouts — the 0.13.2 bundle reported 661 of 661 findings
  unverifiable for that reason alone. A render now consumes the verdicts
  collection recorded instead of producing verdicts it has no repository to
  produce. A checkout that is absent yields `unreachable`, never `stale`, so
  "nothing was checked" stays distinguishable from "the citation no longer
  resolves"; a repository whose report has not been rendered yet writes nothing
  and says nothing (issue #6791).
