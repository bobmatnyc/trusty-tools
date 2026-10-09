Fixed

- The GitHub Issues context source no longer gets HTTP 422 ("The search is longer than 256 characters.") on most real PRs. GitHub bounds the free text of the query (qualifiers excluded, about 2 extra per term), not its raw length, so the keywords are now whitespace-collapsed and cut at a term boundary to a budget of 200 (#9503).
