Fixed
- `build` and `rebuild` refuse items that map to one concept path (`collision`) instead of letting the second overwrite the first, and address every concept by the full ID its ledger record pins, so a tombstone never lands on another concept.
- A tombstone trusty-search cannot detect (frontmatter over 64 KiB) still lands, with a `tombstone_undetectable` error finding, and its index state never reads Withdrawn.
