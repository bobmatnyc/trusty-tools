Fixed

- `tga audit` indexes each repository under the per-checkout id
  `trusty_common::derive_checkout_index_id` derives, the same function
  trusty-review's renderer now calls, instead of a copy of the basename rule.
  Two checkouts of one repository used to collide on a single id, so the sweep
  indexed one tree and the renderer read the other's measurements. The agreement
  between the two crates is a call now rather than a copied rule.
