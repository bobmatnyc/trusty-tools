Changed
- The dashboard schema check knows the `watch_changes` collection, so a store write of the owner's Watch or Unwatch request is validated against `watch_changes.schema.json` instead of refused as a collection with no schema (#10896).
