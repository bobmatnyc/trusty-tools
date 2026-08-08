Fixed

- `service uninstall` removes the unit under its old label too. On a host that
  never ran the migrating install it printed "nothing to do" while leaving
  `com.trusty.trusty-console` loaded — an uninstall that uninstalled nothing
  (#4868)
