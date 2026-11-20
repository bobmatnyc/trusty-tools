Fixed
- A trusty-search index listed under `parked` (cold-parked, `index_not_resident`) now counts as present: `review_pr` and the checkout index resolver pick it instead of reporting "no trusty-search index". The context gate wakes a parked index with one search before reviewing; if that search fails, the review is DEGRADED and the reason names the failed wake.
