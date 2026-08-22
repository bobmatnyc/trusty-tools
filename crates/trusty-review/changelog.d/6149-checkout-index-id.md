Fixed

- `report::derive_index_id` now returns the per-checkout id
  `trusty_common::derive_checkout_index_id` derives (slugified basename plus 8
  hex digits over the canonical path) rather than the bare basename. The
  renderer and the audit that indexed the tree run as separate processes sharing
  only the manifest's checkout path, so both deriving the basename meant a
  machine holding two checkouts of one repository served whichever registered
  first — which is how a report presented another tree's complexity as a
  measurement of the audited one. The out-of-tree component check in
  `report::analyze_scope` stays as the backstop for an index registered under
  the old scheme.
