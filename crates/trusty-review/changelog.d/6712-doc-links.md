Fixed

- The `analyze_enrich` module's doc comment links resolve again — the split that
  created it wrote them as `super::`, which rustdoc reads from the crate root
  rather than from `report/`, so `cargo doc` failed on two broken intra-doc
  links (#6712)
