Fixed

- Register the base checkout behind a worktree with trusty-search, vector lane
  on. Worktree indexes are BM25+KG only since #5060, and nothing ever created
  the base-facet index that owns the embedding lane — so every worktree session
  got lexical and graph search and never semantic. Worktree creation and session
  launch now both ensure it, on a detached thread, and report an unconfirmed
  registration as a warning instead of leaving the absence invisible. Routing a
  worktree's semantic query onto the base index is the other half and is not
  implemented here (#5069).
