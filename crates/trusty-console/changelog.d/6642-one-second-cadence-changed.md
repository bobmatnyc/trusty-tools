Changed
- `--host-sample-interval` defaults to 1 second instead of 5, and the history
  window holds 600 points instead of 120. The span is the same ten minutes and
  the payload still advertises the cadence in use, so an operator who raises the
  interval stretches the span the same 600 points cover
  ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
- `HistorySnapshot.schema_version` is `2`. The payload gained `service_samples`
  and `service_sample_capacity`; a client built against schema 1 parses it fine
  but renders no per-service graph, which is the difference the version
  announces ([#6642](https://github.com/bobmatnyc/trusty-tools/issues/6642)).
