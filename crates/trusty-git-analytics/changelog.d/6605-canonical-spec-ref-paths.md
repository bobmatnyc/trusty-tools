Fixed

- The `audit` module's `# Spec References` blocks name DOC-67 by its
  repo-root-relative path rather than a `../../../../` traversal, which DOC-38
  §2.1 permits only in a Markdown visible section. Eleven references were
  silently unchecked as a result (#6605). `run_full_sweep`'s stage-order note
  moved out of the reference block, where its prose closed the block and left
  two more references unscanned.
