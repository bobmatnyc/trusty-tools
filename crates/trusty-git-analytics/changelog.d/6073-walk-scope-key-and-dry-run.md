Fixed

- The recorded walk scope encodes its branch list as JSON rather than joining on `,`, so a branch whose name contains a comma is no longer indistinguishable from two branches — a run scoped to either used to skip the other's walk. `tga collect --dry-run` also stops printing a full-history-walk skip count: a dry run writes to an empty in-memory database, so that figure was structurally always zero. (#6073)
