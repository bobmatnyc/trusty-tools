Fixed

- `research`'s deployed `skills:` list now matches its source asset — `tm-capabilities` was inherited from `BASE-AGENT` and silently appeared only in the deployed copy (closes [#4643](https://github.com/bobmatnyc/trusty-tools/issues/4643))
  - DOC-42 now documents that `skills:` unions base-first across the `extends:` chain, so a foundation template's declaration is paid for by every descendant
