Added

- A bundle-level technical-debt roll-up (#6781). A sweep, a re-render, and a
  return package now write `report.json` beside `index.md`, carrying counts of
  every declared finding by tier, by dimension, by repository, and by
  tier × dimension, plus the total they all sum to. The tiers are the
  collectors' own `RED` / `AMBER` bands and the dimensions their own
  `dependencies` / `license` / `secrets` / `churn` categories — nothing is
  re-banded or re-labelled.
  - `index.md` gains a "Technical debt by tier" table across repositories,
    rendered from that one computed value rather than counting the findings a
    second time, so the table and `report.json` cannot state different numbers.
