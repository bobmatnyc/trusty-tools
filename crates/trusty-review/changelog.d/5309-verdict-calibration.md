Fixed

- A finding the verifier REFUTED no longer leaves the model's own grade standing: the
  post-verification grade is reconciled in both directions, so a relaxed verdict can no
  longer sit beside an `F` that rested on discarded evidence (#4044).
- The narrative summary is written before the verification round and nothing revisited
  it, so it kept citing refuted findings as merge blockers. A deterministic verification
  notice now leads the review body, naming each refuted finding by the index the prose
  uses (#4044).
- A `code_provable` finding whose own text admits the evidence was not available —
  "the diff does not show their signatures, so this cannot be confirmed from the diff
  alone" — is no longer markable `verified: "confirmed"`. It is pre-stamped
  `unverifiable`, stripped of its escalation signals, and never sent to the verifier.
  This generalises #4081's rule from the claim's subject (registry vocabulary) to the
  claim's own admission, which is what let the defect recur (#5309).
- The verifier can now answer `UNVERIFIABLE`. With only CONFIRMED and REFUTED available
  it had to pick one even for a claim the diff cannot settle, and it picked CONFIRMED
  (#5309).
