Changed

- The LaunchAgent label moved from `com.trusty.trusty-review` onto the
  `com.trusty.<stem>` convention every loaded trusty-* unit obeys, and is read
  from `trusty_common::launchd_labels::REVIEW` rather than restated beside the
  installer's own copy of it. The old label is recorded as a legacy alias, so an
  install evicts a unit left by a prior version instead of running beside it
  (#4868)
- `service install` routes through the label-correct activation path, so the
  `com.trusty.trusty-review.plist` a prior version installed is booted out and
  deleted rather than stranded. Without that, `plist_label_for` moving to
  `com.trusty.review` would leave `tctl start trusty-review` and `tctl stack
  doctor` pointing at a plist that still exists but is no longer written (#4868)
