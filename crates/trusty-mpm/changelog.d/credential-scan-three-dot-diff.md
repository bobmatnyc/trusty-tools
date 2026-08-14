Fixed

- The mandatory pre-push credential scan now scopes its diff with three dots — `git diff origin/main...HEAD` — in both places that state the rule: the composed `WORKFLOW` instruction section and the `tm-workflow` skill's "Git Security Review". Two-dot diffs the two commits, so files DELETED from `main` since the branch point are reported as the branch's own additions
  - measured, not theoretical: one session's two-dot scan returned 19 hits that belonged to another PR's deletions, where three-dot returned zero across 36 files. The same session's earlier pre-first-push scan had been accurate only because `origin/main` still equalled its branch point — correct by luck, and on a busy `main` that luck runs out silently
  - the severity is the noise, not the wording. A credential scan that reports hits nobody owns is one reviewers learn to wave through, and a real secret arrives inside that noise
  - both statements now carry the reason inline, so the next reader does not simplify the third dot back out
