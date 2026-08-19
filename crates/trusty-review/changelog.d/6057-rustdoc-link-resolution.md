Documentation

- Two doc links no longer fail `cargo doc` under
  `-D rustdoc::broken_intra_doc_links`. `contents_links` linked
  `super::polish::collapse_recursive`, a private `fn` that rustdoc cannot resolve
  from a public doc — now plain backticks. `Synthesizer::new` linked a bare
  `with_raw_capture_dir`, which does not resolve because a method is not in scope
  by its own name — now `Self::with_raw_capture_dir`, matching how the same
  method is already referenced elsewhere in the file.
