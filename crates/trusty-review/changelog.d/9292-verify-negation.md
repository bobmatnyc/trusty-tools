Fixed

- The verifier's free-text keyword fallback no longer reads a negated verdict
  as that verdict. "not REFUTED", "cannot be refuted", "isn't refuted",
  "unrefuted" and "not UNVERIFIABLE" now judge nothing, so the finding is
  withheld as unjudged and the verdict keeps its floor; before, a negated
  REFUTED dropped the finding as refuted. A negation counts only within the
  verdict word's own clause, so "Not a real bug. REFUTED." is still a
  refutation (#9292).
