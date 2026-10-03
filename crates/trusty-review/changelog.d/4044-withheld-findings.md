Fixed

- Only findings the verifier confirms are posted. A finding the verifier judges
  unverifiable, or that a hygiene pass marked unverifiable before the round, is
  now withheld instead of posted as an advisory. It is recorded with the reason
  `unverifiable` and counted in `withheld_unverified_count`. An APPROVE or
  APPROVE* review that loses only such advisory findings keeps its verdict
  (owner ruling on #8905, 2026-09-30; #4044).
- `withheld_findings` now records every finding any gate withholds, each with a
  reason that names the gate: `#4044 self-negated (marker "…")`,
  `#4042 citation: …`, `#1873 refuted absence claim: …`, the map-reduce dedup
  and `max_findings` cap, the #8905 citation gate, and the verifier
  (`refuted by the verifier`, `the verifier could not judge it`,
  `past the verifier-call cap`, `unverifiable`, `no verifier`). Before, it recorded #8905
  drops only, and the other paths left only a log line. This holds on both the
  single-pass and the map-reduce path (#4044).
- The self-negation filter now also drops findings that say "this is fine",
  "no issue here" or "non-finding", matched case-insensitively. One such
  finding passed the filter on the 0.37.0 re-measure (#4044).
