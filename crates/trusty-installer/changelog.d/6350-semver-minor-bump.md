Changed
- trusty-installer moves to 0.13.0. `cargo-semver-checks` reports five major
  lints against the published 0.12.0 baseline, all of them already on `main`:
  `pub_module_level_const_missing` (`SUPERVISOR_METRICS_PORT`),
  `inherent_method_missing` and `struct_pub_field_missing`
  (`StubLaunchctl::port_guard_calls` / `refuse_port`) and `trait_method_missing`
  (`LaunchctlPort::port_guard`) from #6349, plus `trait_method_added`
  (`ServiceEnv::evict_retired`) from #6290. For a `0.y.z` crate the breaking
  bump is the MINOR position, so 0.12.1 was never a legal position for that
  work. The root workspace requirement moves from `0.12` to `0.13` (#6350).
