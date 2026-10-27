Fixed
- `every_sample_reaches_a_mid_run_subscriber_exactly_once` ends its writer on the subscriber's progress, so a descheduled subscriber thread no longer fails the mid-run floor on a loaded runner (Refs #10029).
