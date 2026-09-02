Fixed

- Every `# Spec References` block names DOC-50 by its repo-root-relative path
  rather than a `../../../` traversal, so the 25 references they declare are
  checked by `check_sld.sh` instead of skipped (#6605). DOC-38 §2.1 permits a
  file-relative path only in a Markdown visible section.
