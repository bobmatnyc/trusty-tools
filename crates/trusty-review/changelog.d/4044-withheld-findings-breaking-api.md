Breaking

- Behaviour: when no verification round runs — verification disabled in
  config, or no verifier provider could be built — no finding is posted. Every
  finding is withheld in `withheld_findings` with the reason `no verifier`, the
  body leads with "N findings withheld: no verifier (…)", and the review is
  UNKNOWN with no grade and that note as its error. A review with no findings
  keeps its verdict. Before, these findings were posted behind a
  "N findings not verified" note with the verdict unchanged. This supersedes
  the 2026-09-29 #8904 rule (owner ruling, 2026-09-30; #4044).
- Each drop function now takes a `&mut Vec<WithheldFinding>` sink as its last
  argument and records every finding it drops there:
  `finding_hygiene::sanitize_findings`,
  `finding_hygiene::drop_self_negated_or_leaked_findings`,
  `citation_check::enforce_citation_integrity` and
  `absence_claim::drop_refuted_absence_claims`. Pass `&mut Vec::new()` to keep
  the old behaviour (#4044).
- New public fields on structs that are not `#[non_exhaustive]`, so a struct
  literal outside this crate stops compiling:
  `mapreduce::ReducedReview::withheld_findings`, and
  `verify_posted::VerifyReport::unverifiable`, `unconfirmed` and
  `withheld_findings` (#4044).
- `VerifyReport` no longer derives `PartialEq` or `Eq`: its new
  `withheld_findings` holds `Finding`, which implements neither (#4044).
