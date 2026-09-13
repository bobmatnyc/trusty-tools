Fixed

- The eight broken intra-doc links that made `cargo doc -p trusty-mpm` exit 101
  under `#![deny(rustdoc::broken_intra_doc_links)]` now resolve, so the crate's
  docs.rs pages render `build_instructions`,
  `build_system_prompt_for_with_style`,
  `build_system_prompt_for_with_style_and_native` and `DoctorCheck` as
  hyperlinks instead of dead literal text. Three names that no public path can
  reach — the `#[cfg(test)]` counter `ADAPTER_REPROBES_ON_THIS_THREAD` and the
  crate-private `maybe_register_palace_alias` / `palace_alias` — are now written
  as plain code rather than as links that could never resolve (#7351).
