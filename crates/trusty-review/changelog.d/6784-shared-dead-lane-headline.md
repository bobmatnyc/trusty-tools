Fixed

- Both total analyze-lane collapses now lead their Gaps & Caveats line with the
  same "trusty-analyze lane DID NOT RUN" headline. The client-build-failure path
  led with "trusty-analyze data unavailable", which the audit bundle index does
  not recognise, so under `--allow-degraded` a report whose static-analysis lane
  never ran was indexed as one whose lane ran (#6784).
