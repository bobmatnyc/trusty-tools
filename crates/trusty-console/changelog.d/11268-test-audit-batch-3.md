Changed
- The batch-3 test-audit findings in trusty-console are acted on: cache and helper tests that duplicated other coverage are removed, the real-envelope memory health test now runs against a stub socket with no trusty-memory install, and the launchd serve-args test parses the args with the console CLI; no behaviour change (#11268).
