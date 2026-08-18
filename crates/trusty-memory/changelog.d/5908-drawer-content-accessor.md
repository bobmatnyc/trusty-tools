Changed

- Every read of a drawer's body goes through `Drawer::content()` instead of the `Drawer.content` field, which `trusty-common` made private so a derived content hash cannot be desynced by assignment. Behaviour is unchanged (#5902).
