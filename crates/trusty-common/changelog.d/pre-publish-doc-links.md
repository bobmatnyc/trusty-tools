Fixed

- Two broken intra-doc links that failed the pre-publish rustdoc gate
  (`scripts/check_rustdoc_links.sh`): `secret::check_secret`'s doc comment
  pointed at `[\`FilterConfig::apply\`]` without `FilterConfig` in scope, and
  `KgStoreRedb::load_drawers`'s doc comment pointed at
  `[\`load_drawers_with_skipped\`]` without the `Self::` qualifier its sibling
  method needs. Both now resolve — the first via the fully-qualified path
  `crate::memory_core::filter::FilterConfig::apply`, the second via
  `Self::load_drawers_with_skipped`.
