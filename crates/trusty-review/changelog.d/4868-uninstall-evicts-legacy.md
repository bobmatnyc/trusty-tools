Fixed

- `service uninstall` removes the unit under its old label too. The owner's host
  has `com.trusty.trusty-review.plist` on disk, so removing only the canonical
  plist left it behind for a later bootstrap to resurrect (#4868)
