Fixed
- KG pattern extraction now consults WordNet 3.1 part-of-speech membership
  before asserting a triple. An adjective-only object (`is a hard requirement`)
  is rejected, a modifier is walked past to the head noun (`is a fast parser`
  yields `parser`), and an object followed by `of` is rejected as a truncated
  noun phrase (`is an ancestor of origin main`). Words absent from WordNet —
  most crate and tool names — are never rejected for being unknown. (#5399)
