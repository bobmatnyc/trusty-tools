Fixed
- A review whose every finding was withheld posts none of them, and its
  verdict follows the model's (#9188 A; AQ-7t, Bob 2026-10-05). A blocking
  review (REQUEST_CHANGES or BLOCK) is UNKNOWN with no grade and an error, so
  `run --json` exits non-zero. An APPROVE or APPROVE* review keeps its
  verdict and exits 0, whichever gate withheld its findings (citation gate,
  verifier refutation or UNVERIFIABLE, head re-check); it is graded from the
  posted findings alone, so with none it is `A+` for APPROVE and `C+` for
  APPROVE*, and it carries `withheld_count`, `withheld_by_reason` and
  `verdict_status: "no_verified_findings"`. A review whose findings no
  verifier round checked is still UNKNOWN, as before (#4044).
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
- `ReviewResult`, and so `run --json`, gains `withheld_count`,
  `withheld_by_reason` and, when no finding survived, `verdict_status:
  "no_verified_findings"`, whatever the verdict. The MCP envelope gains
  `withheld` (`count`, `by_reason`) and the same `verdict_status`. All are
  absent when nothing was withheld, and `isError` is unchanged (#9188 K).
- Every posted finding is re-checked at the head after the verifier; a
  CONFIRMED finding whose citation does not resolve is withheld (#9188 L).
- `calibrate` reports `unresolvable_survivor_count` and `withheld_by_reason`
  (#9188).
