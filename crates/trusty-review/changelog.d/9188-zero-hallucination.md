Fixed
- A review whose every finding was withheld is now UNKNOWN with no grade,
  including an APPROVE or APPROVE* review that lost only advisory findings and
  a review whose verifier could not confirm any finding (#9188 A). Exit code:
  `run --json` now exits non-zero for such a review, where it returned
  APPROVE with exit 0 before, even when every withheld finding was
  advisory-only.
- A finding with any quoted snippet missing from the cited file is withheld;
  it is no longer kept as `citation_partial` (#9188 B).
- The reviewer's prose and the map-reduce synthesis summary are kept only
  when nothing was withheld and every `path:line` or `path:start-end` they
  cite in a file of the diff overlaps a posted finding's line or `[code: …]`
  span; otherwise the body carries a summary rebuilt from the posted findings
  (#9188 C). Strings such as `127.0.0.1:8080`, `example.com:443` and paths
  outside the diff are not citations, so they no longer replace the prose.
- `[jira:]`, `[gh:]` and `[confluence:]` citations must resolve in the
  context the reviewer was shown (PR title and body, discussion, fetched
  sections), or the finding is withheld (#9188 D). The reference must occur
  as a whole token (`#918` does not match `#9188`), every quoted excerpt must
  occur, and an excerpt shorter than 12 characters verifies nothing.
- A finding must quote the code it describes; neither identifiers in
  unquoted prose nor a bare backtick identifier such as `step_4` or `run()`
  anchors a citation (#9188 E).
- A quote found only on removed lines anchors only a finding about a removal,
  and such a finding, cited at its deletion's position, is posted unchanged;
  it no longer carries a `citation_correction` to the line it already cited
  (#9188 F).
- A cited path with directories resolves only to that path or a file it ends
  at a `/` boundary, never to another file sharing its basename (#9188 H).
- A citation holds only when the quoted code falls on the cited lines; a
  weaker anchor on the line no longer holds it, and a range must contain the
  whole quote (#9188 I).
- On a review that is not UNKNOWN, a withheld finding no longer shapes the
  grade: it is recomputed from the posted findings alone (#9188 J).
- `ReviewResult` gains `withheld_count` and `withheld_by_reason`, and the MCP
  envelope gains `withheld` (`count`, `by_reason`) and, when nothing
  survived, `verdict_status: "no_verified_findings"`; all are absent when
  nothing was withheld, and `isError` is unchanged (#9188 K).
- Every posted finding is re-checked at the head after the verifier; a
  CONFIRMED finding whose citation does not resolve is withheld (#9188 L).
- `calibrate` reports `unresolvable_survivor_count` and `withheld_by_reason`
  (#9188).
