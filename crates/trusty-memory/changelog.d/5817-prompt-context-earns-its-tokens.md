Fixed

`trusty-memory prompt-context` now emits nothing when nothing is relevant, instead of a paragraph explaining that nothing cleared the relevance floor. The `EMPTY_PLACEHOLDER` fallback and the all-withheld notice are both gone; the partial "N further memories withheld" line stays, appended only to a drawer section that already has content.

The injected "Relevant KG facts" section admits hot predicates only (ADR-0028 D7) and is capped at 256 bytes. Storage plumbing — `tag:creator:client=… **tags** drawer:…`, `room:General **contains** drawer:…`, `topic:<commit-sha> **mentioned-in** drawer:…` — is no longer rendered to the model as knowledge. Measured against the live `trusty-tools` palace, 54 of 54 stored triples carry a structural or extraction predicate, so that section was 100% noise by count.

Recalled drawers whose `creator:cwd` provenance tag places them in a different repository are dropped before composition. The filter fails open on an absent tag, an unresolvable session root, or a path it cannot judge, and normalises `.claude/worktrees/<name>` to the checkout that owns it so this project's own worktrees are never mistaken for another project.
