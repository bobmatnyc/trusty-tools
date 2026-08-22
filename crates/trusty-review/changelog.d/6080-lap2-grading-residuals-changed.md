Changed
- The Security Posture prompt no longer calls the findings "lint-graded". They are an LLM's readings of source files with every quote mechanically verified — which the disclaimer directly above the paragraph already said, so the paragraph contradicted it.
- The investigation prompt asks every finding, GREEN included, for a `file` and a verbatim `evidence_quote`, tells the model a GREEN title must name a strength rather than a weakness, and to copy each dimension exactly as the checklist spells it.
