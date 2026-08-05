Fixed

- `plist_label_for` no longer keeps its own table of launchd labels. It kept a
  `com.trusty.<binary>` convention plus hand-added overrides, held in step with
  the daemon crates by grepping their `LAUNCHD_LABEL` constants — and it had
  drifted: it resolved trusty-search to `com.trusty.trusty-search` and stated
  the convention "is correct" for it, while the loaded unit is
  `com.trusty.search`. Every `tctl start`/`stop`/`restart` bootout and every
  `tctl stack doctor` plist-presence check therefore targeted a job that does
  not exist. It now delegates to `trusty_common::launchd_labels`, which the
  daemons read too (#4868)
- The supervisor plist template carried `com.trusty.mpm.supervisor` as its own
  literal beside `PLIST_LABEL`. Two copies of a label are two things that can
  disagree, and a plist whose `Label` key differs from the label the installer
  boots out is the #2827 defect landing on the unit that restarts everything
  else. The template now fills the label from the constant, and the test asserts
  on the rendered output rather than the raw template (#4868)
