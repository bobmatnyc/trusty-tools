Added

- `trusty-audit verify <package.zip> --public-key <file>` checks a received
  handoff package against the retained ed25519 public key. It exits 0 for a
  package whose signature verifies and whose every member still hashes to the
  signed manifest, 3 for one that carries no signature at all, and non-zero
  naming the specific failure for anything else. The verb writes nothing, and
  the key comes only from a file the operator names — never from the engagement
  config, whose private half travelled inside the package it would be checking.
