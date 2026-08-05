Changed

- The LaunchAgent label moved from `com.trusty.trusty-review` onto the
  `com.trusty.<stem>` convention every loaded trusty-* unit obeys, and is read
  from `trusty_common::launchd_labels::REVIEW` rather than restated beside the
  installer's own copy of it. The old label is recorded as a legacy alias, so an
  install evicts a unit left by a prior version instead of running beside it
  (#4868)
