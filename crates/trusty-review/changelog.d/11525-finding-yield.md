Added
- Finding yield per review: every logged review appends its posted and verifier-refuted finding counts to `finding_yield.jsonl` in the review log directory. The MCP `review_health` tool reports them under `finding_yield`: totals, one aggregate per repo, and one per PR (the 50 most recent), each with posted per review and refuted share.
