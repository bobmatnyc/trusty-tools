Fixed

- The verifier's free-text keyword fallback no longer reads a verdict word
  preceded by a negation as that verdict. "not REFUTED", "cannot be refuted",
  "isn't refuted", "unrefuted" and "not UNVERIFIABLE" now judge nothing, so
  the finding is withheld as unjudged and the verdict keeps its floor; before,
  a negated REFUTED dropped the finding as refuted. Only a negation earlier in
  the verdict word's own clause counts: "Not a real bug. REFUTED." and
  "REFUTED is not accurate" are still refutations. A quoted verdict word
  ('REFUTED') still matches (#9292).
