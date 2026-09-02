Fixed

- `sld::parse_inline_refs` no longer drops an inline `# Spec References` entry
  whose path carries a `..` traversal segment or is absolute. It used to
  `continue` past one with no diagnostic, so the reference never reached
  `check_reference` and its anchor was never validated — a whole block could
  name a spec id that exists nowhere and `check_sld.sh` still reported
  `0 error(s)` (#6605). The reader now returns every reference the DOC-38 §2.2
  grammar matches, whatever the path shape, and leaves §2.1 path conformance to
  the linter, which has a `file:line` to report it at. Safe, repo-root-relative
  paths behave exactly as before.
