Fixed

- memory recall ranks by relevance again: L2/L3 scored a candidate `eff_importance * similarity`, but inside one candidate pool `eff_importance` spans ~0.44 where similarity spans ~0.07, so the product ranked by importance and discarded the search. Importance now tilts the score by at most 5% (`IMPORTANCE_TILT`), which is the tiebreaker role its own docs described. Measured on a 1,272-drawer palace with a 400-drawer self-retrieval probe: recall@5 295/400 → 351/400, recall@1 102/400 → 291/400 (closes [#4904](https://github.com/bobmatnyc/trusty-tools/issues/4904))
  - L2 and L3 now share one `rank_score` instead of repeating the expression, so deep recall cannot be left on an older formula
  - every L2 candidate is traced with its score components before the `top_k` truncation, on the `memory_recall_rank` target — the pre-truncation view that tells "never a candidate" apart from "ranked below the cutoff"
