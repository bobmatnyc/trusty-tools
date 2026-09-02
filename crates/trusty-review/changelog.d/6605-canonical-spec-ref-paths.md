Fixed

- `report::exec_summary`'s `# Spec References` block names DOC-67 by its
  repo-root-relative path rather than a `../../../../` traversal, so its
  reference is checked by `check_sld.sh` instead of skipped (#6605).
