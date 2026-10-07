Changed
- A D or F grade is now a hard floor on the verdict, the follow-up the
  map-reduce grade-floor entry promised: a review graded D+, D or D- reads at
  least REQUEST_CHANGES and one graded F reads BLOCK, whatever relaxes it. A
  confirmed low-confidence finding, an advisory-only finding set, the
  verifier's re-check, a self-reported BLOCK held back on one uncited High
  finding, and findings dropped before grading no longer lower it. The floor
  reads the reviewer's own grade, before any pass regrades it; grades A to C
  behave as before. With synthesis off or not answering, the strictest chunk
  grade floors a map-reduce review; when synthesis answers, only its grade
  does. Two cases stay REQUEST_CHANGES: a review whose every finding was
  withheld (`suppressed_reject`), and an F whose every blocker the verifier
  refuted, since the F is withdrawn with them. A review the floor raises reads
  `verdict_status` `parsed`: an F review whose blocker a citation gate
  withheld, and which still posts a finding, reads BLOCK / `parsed`, not
  REQUEST_CHANGES / `suppressed_reject` (#9310).
- What changes is the `verdict` and `verdict_status` fields of the JSON and
  MCP result and the verdict in the PR comment heading. The `trusty-review run` exit code does
  not change: it reads only a skipped run or a recorded error, never the
  verdict (#9310).
