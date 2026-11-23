Fixed
- `search_code` compact hits no longer carry an A–F `grade`. The letter was thresholded from a rank-only RRF score, whose maximum (`1.0/61`) fell below the "B" cut, so the best possible hybrid hit could only grade "C" while a near-orthogonal cosine on the vector-only fallback graded "A". `score` and `match_reason` are unchanged.
