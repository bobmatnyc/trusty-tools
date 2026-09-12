Documentation

- Two dead intra-doc links in `commands/guided.rs` now resolve (#7534, #7396).
  `fallback_protected` still linked `launch_protected_workspace` by bare name
  after #7603 moved it to `guided_protected`, and `fallback_protected_gated`
  named a `provision_for_fallback_gated` that has never existed — the gate is a
  parameter of `provision_for_fallback`, not a second function. Both now use
  the module-path form that resolves in a bin target.
