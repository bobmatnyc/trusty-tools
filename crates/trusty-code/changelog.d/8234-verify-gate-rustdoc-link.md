Fixed

- **`verify_gate` module docs resolve their own link again (#8234).** The
  `detect::detect_test_command` intra-doc link in the module header is now
  backed by a `crate::verify_gate::…` reference definition, so rustdoc resolves
  it from the crate root instead of failing the intra-doc link gate.
