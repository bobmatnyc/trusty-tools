Fixed

- The product name in each dashboard's sidebar is readable in both themes.
  `.brand-title` in ui-search and ui-memory used `--trusty-text-inverse`, the
  contrast colour for a rust fill, which flips to `#201612` in the dark palette
  — near-black on the `#171009` sidebar, about 1.1:1, so "Trusty Search" and
  "Trusty Memory" were invisible in dark. ui-analyze had the mirror of it,
  taking the page body colour `--text`, which on the light palette is `#2b1c12`
  against the same always-dark sidebar. All three now use
  `--trusty-sidebar-text` (`#e6d8c8` in both palettes), the token every other
  label on that surface already uses
  ([#7589](https://github.com/bobmatnyc/trusty-tools/issues/7589)).
