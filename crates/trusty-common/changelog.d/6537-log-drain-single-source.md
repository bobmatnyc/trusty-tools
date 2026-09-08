Added

- **`log_drain::single_source` consolidates the single-project drain shape
  trusty-agents and trusty-code each reimplemented (#6537).** New
  `SingleSourceSection` (the 8-field `[log_drain]`/`log_drain.yaml` shape),
  `resolve_single_source`, `DrainOutcome`, and `run_tick` give every
  single-project daemon one resolver and one scheduler tick instead of a
  per-crate copy; each consumer keeps only its file-format adapter (a TOML
  section or a standalone YAML file) and names its own crate/include glob.
  `run_tick` takes the local idempotency-manifest-cache directory as a
  parameter rather than resolving it internally, so callers (and their
  tests) never touch a real home directory implicitly. trusty-mpm's own
  multi-source `sources[]` resolver is not part of this consolidation — see
  epic #6533's follow-up.
