Changed
- The batch-3 test-audit findings in trusty-analyze are acted on: tests that only exercised a derive or another crate's registry are removed, and serde round-trip tests now compare against a JSON literal so a renamed field, tag or variant fails; no behaviour change (#11268).
