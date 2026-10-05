Changed
- `citation_gate::GateReport::partial` is deprecated: #9188 B withholds a
  partly quoted finding instead of keeping it, so read `dropped` and
  `withheld_findings` (`missing_fragment`). It still counts the kept findings
  marked `citation_partial` (#9188).
