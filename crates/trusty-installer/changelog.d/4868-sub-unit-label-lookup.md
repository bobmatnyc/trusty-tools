Fixed

- `plist_label_for` consults the service registry before falling back to the
  naming convention, so a member whose only unit is a SUB-unit resolves
  correctly — `trusty-agents` returned `com.trusty.agents` while the loaded unit
  is `com.trusty.agents.slack`, which would have had `tctl` target a job that
  does not exist (#4868)
