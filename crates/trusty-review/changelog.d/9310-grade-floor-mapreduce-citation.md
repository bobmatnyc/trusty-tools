Fixed
- On the map-reduce path the reviewer's grade now counts toward the verdict.
  A synthesis APPROVE graded D reads REQUEST_CHANGES and one graded F reads
  BLOCK, unless a confirmed low-confidence finding relaxes it; a follow-up
  #9310 PR makes a D or F grade a hard floor that no override relaxes. The
  grade is kept; before, it read APPROVE and its grade was raised into the
  APPROVE band. The grade only tightens the verdict: an APPROVE graded B+
  beside Medium findings stays APPROVE (#9310).
- With synthesis off or not answering, a chunk reply of APPROVE graded D or F
  is a rejection, so one such chunk makes the whole review REQUEST_CHANGES or
  BLOCK. When every finding of that chunk is withheld, the review reads
  REQUEST_CHANGES / `suppressed_reject`, not APPROVE / `all_withheld` (#9310).
- With synthesis on, a chunk reply of APPROVE graded F makes the mechanical
  verdict BLOCK, so the synthesis floor holds the review at REQUEST_CHANGES or
  stricter even when synthesis answers APPROVE. When every finding of that
  chunk is withheld, the review reads `suppressed_reject` (#9310).
- A finding's `source_citation` qualifies a High finding for the BLOCK floor
  only when the whole string is a citation, optionally ending in one `.`: an
  optional `code:`, `jira:` or `gh:` prefix in any case, then a
  `path:line[:col][-line]`, a ticket key or spec id (`SPEC-X-03~draft`),
  `[owner/repo]#N` or a named spec section, or a `,`/`;` list of these. A
  path may contain `+ @ [ ] ( ) ~` and may be backticked. An identifier
  inside prose ("see #1 trust me"), `#0`, `path:0` and a bare `§1` no longer
  qualify, so such a finding is held at the REQUEST_CHANGES tier instead of
  BLOCK (#9310).
- The `trusty-review run` exit code does not change: it reads only a skipped
  run or a recorded error, never the verdict (#9310).
