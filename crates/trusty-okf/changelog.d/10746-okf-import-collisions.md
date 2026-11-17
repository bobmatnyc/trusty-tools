Fixed
- `build` and `rebuild` refuse items that map to one concept path (`collision`) instead of letting the second overwrite the first, and address every concept by the full ID its ledger record pins, so a tombstone never lands on another concept.
- A tombstone trusty-search cannot detect (frontmatter over 64 KiB) still lands, with a `tombstone_undetectable` error finding, and its index state never reads Withdrawn.
- `import run --feed` checks the state directory and index ID before any write; a failure after the import has written prints the report and exits 1, never 2. A source item that is no longer a regular file (for example a FIFO swapped in after the plan) is refused `unsupported_format` instead of blocking the run.
