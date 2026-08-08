Changed

- `LAUNCHD_LABEL` is read from `trusty_common::launchd_labels::ANALYZE` rather
  than restated beside the installer's separate copy of it. The value is
  unchanged — the point is that the installer's copy can no longer drift away
  from the daemon's, which is what broke trusty-search (#4868)
