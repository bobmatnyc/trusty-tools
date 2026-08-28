Added

- `launchd_labels::RETIRED_SERVICES` records units a current install must EVICT
  but never write, with `retired_service_for_member` and
  `retired_labels_for_member` to read it.
- `trusty-review`'s row moved there from `SERVICES` (#6290): the daemon is
  retired, but `com.trusty.review` is still loaded on every host that ran the
  old binary, so the row has to survive for anything to boot it out.
