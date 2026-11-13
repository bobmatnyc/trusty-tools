Fixed
- The dashboard store refuses a `set`, `update` or batch that writes `meta/settings` `technical_level` or an item's `level_override` outside the six technical levels, whatever the schema mode. A refused op aborts the batch and leaves the store unchanged (#11064).
