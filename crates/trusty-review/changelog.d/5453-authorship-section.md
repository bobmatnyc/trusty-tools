Added

- The technical due-diligence report gains an "Authorship & Key-Person Risk"
  section (#5453, #6004): bus factor, ownership concentration, single-author
  subsystems, and a trailing-12-month trajectory per application, plus a new
  `authorship_summary` LLM narrative slot (high-level, health-from-an-
  authorship-perspective framing) on the existing synthesis call, inheriting
  its numeric guardrail. Key-man risks render in this dedicated section, not
  scattered across Top Risks. The manifest's per-repository entries accept a
  new `authorship` key naming the JSON artifact `tga audit` writes; a
  repository whose artifact fails to load states that as a named gap for that
  repository only — never a silently absent section, never an aborted build.
  The Key Facts block's author-count and trajectory rows, left as named gaps
  by the earlier Code/Security/Performance change, now populate once
  authorship data exists.
