Documentation

- Two broken intra-doc links in the `tools::embed_audit` module doc, which
  failed the pre-publish rustdoc gate (`scripts/check_rustdoc_links.sh`).
  `EmbedHealth` is not imported here, so the link now names its full path,
  `trusty_common::memory_core::retrieval::EmbedHealth::missing_vector_ids`. The
  reference to `MAX_PALACES_IN_REPORT` is now plain code text and the link
  points at `console_metrics` instead: the constant is private to that module,
  so no link to it can resolve from public documentation.
