Fixed
- A `review_pr` miss on an index the trusty-search daemon lists as parked now says the index is parked and points at trusty-search `ensure_project`, instead of telling the operator to index the checkout (#11417). `SearchClient` gains `list_parked_index_ids`, which defaults to none.
