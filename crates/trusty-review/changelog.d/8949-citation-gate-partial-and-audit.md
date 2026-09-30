Changed

- The citation gate keeps a finding when one quoted snippet places its
  citation and another is not in the diff. The finding is marked
  `citation_partial`, demoted to advisory so it cannot drive the verdict,
  noted as "citation partly unverified", and posted in the review body, never
  inline. A finding none of whose quoted snippets is in the file still drops.
- A double-quoted phrase in a finding's prose no longer has to appear in the
  cited file. It anchors the citation when present and is ignored otherwise.
  Quote pairing skips backtick spans, so a string literal in quoted code no
  longer opens a prose quote.
- Every finding the gate drops is kept in the review record as
  `withheld_findings`, with its reason and the quoted fragment that failed to
  match. The drop log line names that fragment.
- An APPROVE* review stays APPROVE* when the gate drops only advisory findings,
  instead of becoming UNKNOWN.
- New public type `models::WithheldFinding`.
